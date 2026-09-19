use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};

use crate::client::AgentClient;
use crate::contract::{NativeContract, PairingEnvelope};
use crate::error::{AgentError, AgentResult};
use crate::process::{start_bridge_process, stop_owned_process};
use crate::server::{BridgeServerOptions, GifGunBridgeServer};
use crate::session::{
    AgentSessionRecord, SessionStatus, SessionStore, StartupRecord, random_secret,
};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const STARTUP_POLL: Duration = Duration::from_millis(100);
const RENDER_POLL: Duration = Duration::from_millis(500);

#[derive(Debug, Parser)]
#[command(
    name = "gifgun-agent",
    version,
    about = "Private local bridge for GifGun Online"
)]
pub struct Arguments {
    #[arg(long, global = true)]
    session: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Pair,
    Status,
    Capabilities,
    State,
    Call {
        capability: String,
        #[arg(long)]
        revision: Option<u64>,
    },
    Upload {
        path: PathBuf,
        #[arg(long, value_enum, default_value_t = UploadPurpose::Source)]
        purpose: UploadPurpose,
        #[arg(long)]
        layer: Option<String>,
    },
    Render {
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        overwrite: bool,
    },
    Save {
        path: PathBuf,
        #[arg(long)]
        overwrite: bool,
    },
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    Disconnect,
    #[command(hide = true)]
    Serve {
        #[arg(long)]
        startup: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum UploadPurpose {
    #[default]
    Source,
    Media,
    Replace,
}

#[derive(Debug, Subcommand)]
enum ProjectCommand {
    Open {
        path: PathBuf,
    },
    Save {
        path: PathBuf,
        #[arg(long)]
        overwrite: bool,
    },
}

pub async fn run() -> AgentResult<()> {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    if arguments.len() == 2 && matches!(arguments[1].to_str(), Some("--version" | "-V")) {
        let contract = NativeContract::load_embedded()?;
        println!(
            "gifgun-agent {}\ncontract {}",
            env!("CARGO_PKG_VERSION"),
            contract.digest()
        );
        return Ok(());
    }
    run_with(Arguments::parse_from(arguments)).await
}

async fn run_with(arguments: Arguments) -> AgentResult<()> {
    match arguments.command {
        Command::Pair => pair().await,
        Command::Serve { startup } => serve(startup).await,
        Command::Status => status(arguments.session.as_deref()).await,
        Command::Capabilities => {
            invoke_selected(
                arguments.session.as_deref(),
                "editor.get_capabilities",
                json!({}),
                None,
            )
            .await
        }
        Command::State => {
            invoke_selected(
                arguments.session.as_deref(),
                "editor.get_state",
                json!({}),
                None,
            )
            .await
        }
        Command::Call {
            capability,
            revision,
        } => {
            let input = read_stdin_json()?;
            invoke_selected(arguments.session.as_deref(), &capability, input, revision).await
        }
        Command::Upload {
            path,
            purpose,
            layer,
        } => upload(arguments.session.as_deref(), &path, purpose, layer).await,
        Command::Render { output, overwrite } => {
            render(arguments.session.as_deref(), output.as_deref(), overwrite).await
        }
        Command::Save { path, overwrite } => {
            save(arguments.session.as_deref(), &path, overwrite).await
        }
        Command::Project { command } => match command {
            ProjectCommand::Open { path } => {
                project_open(arguments.session.as_deref(), &path).await
            }
            ProjectCommand::Save { path, overwrite } => {
                project_save(arguments.session.as_deref(), &path, overwrite).await
            }
        },
        Command::Disconnect => disconnect(arguments.session.as_deref()).await,
    }
}

async fn pair() -> AgentResult<()> {
    let raw = read_stdin()?;
    if raw.is_empty() {
        return Err(AgentError::Cli(
            "paste the GifGun pairing instruction on stdin".into(),
        ));
    }
    let contract = NativeContract::load_embedded()?;
    let (envelope, envelope_value) = decode_instruction(&contract, &raw)?;
    if envelope.expires_at <= now_millis() {
        return Err(AgentError::Cli(
            "the GifGun pairing instruction has expired".into(),
        ));
    }
    contract
        .check_compatibility(&envelope.compatibility)
        .map_err(|_| {
            AgentError::Cli(
                "this native bridge is incompatible with the open GifGun tab; download the required version and retry pairing".into(),
            )
        })?;

    let store = SessionStore::discover()?;
    if let Ok(known) = store.read_session(None)
        && AgentClient::new(known)?
            .get("/v1/agent/status")
            .await
            .is_ok()
    {
        return Err(AgentError::Cli(
            "disconnect the current GifGun agent session before pairing again".into(),
        ));
    }
    let mut session =
        AgentSessionRecord::new(random_secret(18)?, envelope.port, random_secret(32)?);
    store.write_session(&session)?;
    if let Err(error) = store.set_current(&session.session_id) {
        let _ = store.remove_session(&session.session_id);
        return Err(error);
    }
    let startup = StartupRecord {
        envelope: envelope_value,
        session: session.clone(),
    };
    let startup_path = match store.write_startup(&startup) {
        Ok(path) => path,
        Err(error) => {
            let _ = store.remove_session(&session.session_id);
            return Err(error);
        }
    };
    let executable = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            let _ = store.remove_session(&session.session_id);
            return Err(error.into());
        }
    };
    let mut child = match start_bridge_process(
        &executable,
        &startup_path,
        &store.log_path(&session.session_id),
    ) {
        Ok(child) => child,
        Err(error) => {
            let _ = store.remove_session(&session.session_id);
            return Err(error);
        }
    };
    session.pid = Some(child.id());
    if let Err(error) = store.write_session(&session) {
        let _ = stop_owned_process(&mut child);
        let _ = store.remove_session(&session.session_id);
        return Err(error);
    }

    let client = match AgentClient::new(session.clone()) {
        Ok(client) => client,
        Err(error) => {
            let _ = stop_owned_process(&mut child);
            let _ = store.remove_session(&session.session_id);
            return Err(error);
        }
    };
    match wait_for_bridge(&client, &mut child).await {
        Ok(bridge_status) => {
            session.status = SessionStatus::Waiting;
            store.write_session(&session)?;
            output(json!({
                "sessionId": session.session_id,
                "port": session.port,
                "status": bridge_status.get("status").cloned().unwrap_or(Value::String("waiting".into())),
                "next": "Approve the detected local agent in the open GifGun tab."
            }))
        }
        Err(error) => {
            let _ = stop_owned_process(&mut child);
            let _ = store.remove_session(&session.session_id);
            Err(error)
        }
    }
}

async fn serve(startup_path: PathBuf) -> AgentResult<()> {
    let store = SessionStore::discover()?;
    let startup = store.consume_startup(&startup_path)?;
    let contract = NativeContract::load_embedded()?;
    contract
        .validate_pairing(&startup.envelope)
        .map_err(|_| AgentError::Cli("startup pairing envelope is invalid".into()))?;
    let envelope: PairingEnvelope = serde_json::from_value(startup.envelope)
        .map_err(|_| AgentError::Cli("startup pairing envelope is malformed".into()))?;
    let mut session = startup.session;
    let server = match GifGunBridgeServer::start(BridgeServerOptions {
        port: envelope.port,
        envelope,
        agent_token: session.agent_token.clone(),
        session_id: session.session_id.clone(),
        command_timeout: Duration::from_secs(60),
        session_expires_at: session.expires_at,
    })
    .await
    {
        Ok(server) => server,
        Err(error) => {
            let _ = store.remove_session(&session.session_id);
            return Err(error);
        }
    };
    session.pid = Some(std::process::id());
    session.status = SessionStatus::Waiting;
    if let Err(error) = store.write_session(&session) {
        let _ = server.close().await;
        let _ = store.remove_session(&session.session_id);
        return Err(error);
    }

    let shutdown = server.shutdown_handle();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            shutdown.close().await;
        }
    });
    let result = server.wait().await;
    let cleanup = store.remove_session(&session.session_id);
    result?;
    cleanup
}

async fn status(selected: Option<&str>) -> AgentResult<()> {
    let store = SessionStore::discover()?;
    let mut session = store.read_session(selected)?;
    let value = AgentClient::new(session.clone())?
        .get("/v1/agent/status")
        .await?;
    session.status = match value.get("status").and_then(Value::as_str) {
        Some("connected") => SessionStatus::Connected,
        Some("waiting" | "approval_pending") => SessionStatus::Waiting,
        _ => SessionStatus::Stopped,
    };
    store.write_session(&session)?;
    output(value)
}

async fn invoke_selected(
    selected: Option<&str>,
    capability_id: &str,
    input: Value,
    expected_revision: Option<u64>,
) -> AgentResult<()> {
    let store = SessionStore::discover()?;
    let session = store.read_session(selected)?;
    let client = AgentClient::new(session)?;
    let contract = NativeContract::load_embedded()?;
    output(send_command(&client, &contract, capability_id, input, expected_revision).await?)
}

async fn send_command(
    client: &AgentClient,
    contract: &NativeContract,
    capability_id: &str,
    input: Value,
    expected_revision: Option<u64>,
) -> AgentResult<Value> {
    contract
        .validate_capability_input(capability_id, &input)
        .map_err(|error| AgentError::Cli(error.message.into()))?;
    let mut envelope = json!({
        "protocol": contract.protocol(),
        "contractDigest": contract.digest(),
        "requestId": random_secret(18)?,
        "capabilityId": capability_id,
        "input": input,
    });
    if let Some(revision) = expected_revision {
        envelope["expectedRevision"] = json!(revision);
    }
    contract
        .validate_command(&envelope)
        .map_err(|error| AgentError::Cli(error.message.into()))?;
    let response = client.post("/v1/agent/command", envelope).await?;
    contract
        .validate_result(&response)
        .map_err(|_| AgentError::Bridge("the browser returned an invalid command result".into()))?;
    Ok(response)
}

async fn upload(
    selected: Option<&str>,
    path: &Path,
    purpose: UploadPurpose,
    layer: Option<String>,
) -> AgentResult<()> {
    let store = SessionStore::discover()?;
    let client = AgentClient::new(store.read_session(selected)?)?;
    let contract = NativeContract::load_embedded()?;
    let path = path_for_bridge(path)?;
    let reservation = client
        .post("/v1/agent/transfers/upload", json!({"path": path}))
        .await?;
    let transfer_id = response_string(&reservation, "transferId")?;
    let state = send_command(&client, &contract, "editor.get_state", json!({}), None).await?;
    let revision = response_revision(&state)?;
    let (capability, input) = match purpose {
        UploadPurpose::Source => {
            let source_loaded = !state
                .get("result")
                .and_then(|result| result.get("source"))
                .is_none_or(Value::is_null);
            (
                if source_loaded {
                    "project.replace_source"
                } else {
                    "project.load_source"
                },
                json!({"transferId": transfer_id}),
            )
        }
        UploadPurpose::Media => ("layer.add_media", json!({"transferId": transfer_id})),
        UploadPurpose::Replace => {
            let layer = layer.ok_or_else(|| {
                AgentError::Cli("replacing media requires --layer <layer-id>".into())
            })?;
            (
                "layer.replace_media",
                json!({"layerId": layer, "transferId": transfer_id}),
            )
        }
    };
    output(send_command(&client, &contract, capability, input, Some(revision)).await?)
}

async fn reserve_output(client: &AgentClient, path: &Path, overwrite: bool) -> AgentResult<String> {
    let path = path_for_bridge(path)?;
    let reservation = client
        .post(
            "/v1/agent/transfers/output",
            json!({"path": path, "overwrite": overwrite}),
        )
        .await?;
    Ok(response_string(&reservation, "transferId")?.to_owned())
}

async fn save(selected: Option<&str>, path: &Path, overwrite: bool) -> AgentResult<()> {
    let store = SessionStore::discover()?;
    let client = AgentClient::new(store.read_session(selected)?)?;
    let contract = NativeContract::load_embedded()?;
    let transfer_id = reserve_output(&client, path, overwrite).await?;
    output(
        send_command(
            &client,
            &contract,
            "render.save",
            json!({"transferId": transfer_id}),
            None,
        )
        .await?,
    )
}

async fn wait_for_project_file(
    client: &AgentClient,
    contract: &NativeContract,
    mut state: Value,
) -> AgentResult<Value> {
    if state.get("ok").and_then(Value::as_bool) != Some(true) {
        return Ok(state);
    }
    loop {
        let status = state
            .get("result")
            .and_then(|result| result.get("status"))
            .and_then(Value::as_str);
        if matches!(status, Some("completed" | "failed" | "canceled")) {
            return Ok(state);
        }
        tokio::time::sleep(RENDER_POLL).await;
        state = send_command(client, contract, "project.get_file_status", json!({}), None).await?;
    }
}

async fn project_open(selected: Option<&str>, path: &Path) -> AgentResult<()> {
    let store = SessionStore::discover()?;
    let client = AgentClient::new(store.read_session(selected)?)?;
    let contract = NativeContract::load_embedded()?;
    let path = path_for_bridge(path)?;
    let reservation = client
        .post("/v1/agent/transfers/upload", json!({"path": path}))
        .await?;
    let transfer_id = response_string(&reservation, "transferId")?;
    let state = send_command(&client, &contract, "editor.get_state", json!({}), None).await?;
    let started = send_command(
        &client,
        &contract,
        "project.open_file",
        json!({"transferId": transfer_id, "confirmation": true}),
        Some(response_revision(&state)?),
    )
    .await?;
    output(wait_for_project_file(&client, &contract, started).await?)
}

async fn project_save(selected: Option<&str>, path: &Path, overwrite: bool) -> AgentResult<()> {
    let store = SessionStore::discover()?;
    let client = AgentClient::new(store.read_session(selected)?)?;
    let contract = NativeContract::load_embedded()?;
    let transfer_id = reserve_output(&client, path, overwrite).await?;
    let state = send_command(&client, &contract, "editor.get_state", json!({}), None).await?;
    let started = send_command(
        &client,
        &contract,
        "project.save_file",
        json!({"transferId": transfer_id}),
        Some(response_revision(&state)?),
    )
    .await?;
    output(wait_for_project_file(&client, &contract, started).await?)
}

async fn render(
    selected: Option<&str>,
    output_path: Option<&Path>,
    overwrite: bool,
) -> AgentResult<()> {
    let store = SessionStore::discover()?;
    let client = AgentClient::new(store.read_session(selected)?)?;
    let contract = NativeContract::load_embedded()?;
    let state = send_command(&client, &contract, "editor.get_state", json!({}), None).await?;
    let mut terminal = send_command(
        &client,
        &contract,
        "render.start",
        json!({}),
        Some(response_revision(&state)?),
    )
    .await?;
    if terminal.get("ok").and_then(Value::as_bool) != Some(true) {
        return output(terminal);
    }
    loop {
        let status = terminal
            .get("result")
            .and_then(|result| result.get("status"))
            .and_then(Value::as_str);
        if matches!(status, Some("completed" | "failed" | "canceled")) {
            break;
        }
        tokio::time::sleep(RENDER_POLL).await;
        terminal = send_command(&client, &contract, "render.get_status", json!({}), None).await?;
    }
    if terminal
        .get("result")
        .and_then(|result| result.get("status"))
        .and_then(Value::as_str)
        == Some("completed")
        && let Some(path) = output_path
    {
        let transfer_id = reserve_output(&client, path, overwrite).await?;
        terminal = send_command(
            &client,
            &contract,
            "render.save",
            json!({"transferId": transfer_id}),
            None,
        )
        .await?;
    }
    output(terminal)
}

async fn disconnect(selected: Option<&str>) -> AgentResult<()> {
    let store = SessionStore::discover()?;
    let session = store.read_session(selected)?;
    let client = AgentClient::new(session.clone())?;
    let _ = client.post("/v1/agent/disconnect", json!({})).await;
    store.remove_session(&session.session_id)?;
    output(json!({"sessionId": session.session_id, "status": "disconnected"}))
}

async fn wait_for_bridge(
    client: &AgentClient,
    child: &mut std::process::Child,
) -> AgentResult<Value> {
    let deadline = tokio::time::Instant::now() + STARTUP_TIMEOUT;
    loop {
        if child.try_wait()?.is_some() {
            return Err(AgentError::Bridge(
                "the native bridge could not start; the selected loopback port may be unavailable"
                    .into(),
            ));
        }
        match client.get("/v1/agent/status").await {
            Ok(value) => return Ok(value),
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(STARTUP_POLL).await;
            }
            Err(error) => return Err(error),
        }
    }
}

pub fn decode_instruction(
    contract: &NativeContract,
    input: &str,
) -> AgentResult<(PairingEnvelope, Value)> {
    let trimmed = input.trim();
    let lowercase = trimmed.to_ascii_lowercase();
    let encoded = lowercase
        .find("pairing envelope:")
        .map(|index| &trimmed[index + "pairing envelope:".len()..])
        .unwrap_or(trimmed)
        .trim();
    let decoded = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| AgentError::Cli("the pairing instruction is malformed".into()))?;
    let value: Value = serde_json::from_slice(&decoded)
        .map_err(|_| AgentError::Cli("the pairing instruction is malformed".into()))?;
    contract.validate_pairing(&value).map_err(|_| {
        AgentError::Cli("the pairing instruction does not match this bridge".into())
    })?;
    let envelope = serde_json::from_value(value.clone())
        .map_err(|_| AgentError::Cli("the pairing instruction is malformed".into()))?;
    Ok((envelope, value))
}

fn read_stdin() -> AgentResult<String> {
    let mut value = String::new();
    std::io::stdin().read_to_string(&mut value)?;
    Ok(value.trim().to_owned())
}

fn read_stdin_json() -> AgentResult<Value> {
    let input = read_stdin()?;
    if input.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&input)
        .map_err(|_| AgentError::Cli("stdin must contain one valid JSON value".into()))
}

fn output(value: Value) -> AgentResult<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &value)
        .map_err(|_| AgentError::Cli("command output could not be serialized".into()))?;
    stdout.write_all(b"\n")?;
    Ok(())
}

fn response_string<'a>(value: &'a Value, field: &str) -> AgentResult<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| AgentError::Bridge("the local bridge response is incomplete".into()))
}

fn response_revision(value: &Value) -> AgentResult<u64> {
    value
        .get("revision")
        .and_then(Value::as_u64)
        .ok_or_else(|| AgentError::Bridge("the browser response has no revision".into()))
}

fn path_for_bridge(path: &Path) -> AgentResult<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| AgentError::Cli("the local path is not valid UTF-8".into()))
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
