use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;

use gifgun_agent::contract::NativeContract;
use gifgun_agent::server::{BridgeServerOptions, GifGunBridgeServer, LauncherStatus};
use gifgun_agent::session::{AgentSessionRecord, SessionStore, StartupRecord};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use tempfile::tempdir;

const ORIGIN: &str = "https://gifgun.test";
const PAIR: &str = "synthetic-pairing-token-12345678901234567890";
const AGENT: &str = "synthetic-agent-token-12345678901234567890";

#[tokio::test]
async fn handoff_requires_browser_approval_and_tracks_the_current_stream() {
    let server = GifGunBridgeServer::start(BridgeServerOptions::for_test(0, ORIGIN, PAIR, AGENT))
        .await
        .unwrap();
    let client = Client::new();
    let mut status = server.launcher_status();
    assert_eq!(*status.borrow(), LauncherStatus::Waiting);
    let route = server.url("/v1/browser/handoff");
    assert_eq!(
        client
            .get(&route)
            .header("Origin", ORIGIN)
            .bearer_auth(PAIR)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(&route)
            .header("Origin", "https://other.test")
            .bearer_auth(AGENT)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let approved: Value = client
        .post(server.url("/v1/pair/approve"))
        .header("Origin", ORIGIN)
        .bearer_auth(PAIR)
        .json(&json!({"compatibility": NativeContract::load_embedded().unwrap().compatibility()}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = approved["browserToken"].as_str().unwrap();
    assert_eq!(
        client
            .get(&route)
            .header("Origin", ORIGIN)
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    let first = client
        .get(server.url("/v1/browser/stream"))
        .header("Origin", ORIGIN)
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let _ = status
        .wait_for(|state| *state == LauncherStatus::Connected)
        .await
        .unwrap();
    let handoff: Value = client
        .get(&route)
        .header("Origin", ORIGIN)
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let text = handoff["instruction"].as_str().unwrap();
    assert_eq!(
        text,
        gifgun_agent::handoff::instruction("session-test").unwrap()
    );
    assert!(!text.contains(token));
    assert!(!text.contains(PAIR));
    assert!(!text.contains(AGENT));
    assert_eq!(
        client
            .get(&route)
            .header("Origin", ORIGIN)
            .bearer_auth(AGENT)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let second = client
        .get(server.url("/v1/browser/stream"))
        .header("Origin", ORIGIN)
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    drop(first);
    assert_eq!(
        client
            .get(&route)
            .header("Origin", ORIGIN)
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    drop(second);
    let _ = status
        .wait_for(|state| *state == LauncherStatus::Reconnecting)
        .await
        .unwrap();
    assert_eq!(
        client
            .get(&route)
            .header("Origin", ORIGIN)
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    server.close().await.unwrap();
    assert_eq!(*status.borrow(), LauncherStatus::Stopped);
}

#[test]
fn owned_daemon_stops_and_removes_its_record_after_console_owner_loss() {
    let directory = tempdir().unwrap();
    let store = SessionStore::at(directory.path());
    let mut session = AgentSessionRecord::new("session-owned".into(), 0, AGENT.into());
    session.workspace_id = Some("workspace-owned".into());
    store.write_session(&session).unwrap();
    let startup = store
        .write_startup(&StartupRecord {
            envelope: Value::Null,
            session: session.clone(),
            launch_origin: Some(ORIGIN.into()),
        })
        .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_gifgun-agent"))
        .args(["serve", "--ready", "--owned", "--startup"])
        .arg(startup)
        .env("GIFGUN_AGENT_CACHE_DIR", directory.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let ready: Value = serde_json::from_str(&line).unwrap();
    child.stdin.as_mut().unwrap().write_all(b"ready\n").unwrap();
    line.clear();
    reader.read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<LauncherStatus>(&line).unwrap(),
        LauncherStatus::Waiting
    );
    let port = ready["port"].as_u64().unwrap() as u16;
    let mut stalled = BufReader::new(std::net::TcpStream::connect(("127.0.0.1", port)).unwrap());
    write!(stalled.get_mut(), "POST /v1/agent/command HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {AGENT}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nExpect: 100-continue\r\n\r\n", b"{}".len()).unwrap();
    line.clear();
    stalled.read_line(&mut line).unwrap();
    assert_eq!(line, "HTTP/1.1 100 Continue\r\n");
    // Keep the body pending. It must not keep this daemon alive after owner loss.
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
    assert!(!store.session_path(&session.session_id).exists());
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", ready["port"].as_u64().unwrap() as u16))
            .is_err()
    );
}

struct Console {
    child: Child,
    lines: mpsc::Receiver<String>,
}
impl Console {
    fn start(cache: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_gifgun-agent"))
            .args(["launch", "--no-browser"])
            .env("GIFGUN_AGENT_CACHE_DIR", cache)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let reader = BufReader::new(child.stdout.take().unwrap());
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in reader.lines() {
                let Ok(line) = line else {
                    break;
                };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Self { child, lines }
    }
    fn line(&self, prefix: &str) -> String {
        loop {
            let line = self.lines.recv().unwrap();
            if let Some(value) = line.strip_prefix(prefix) {
                return value.to_owned();
            }
        }
    }
}
impl Drop for Console {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

enum ConsoleStop {
    Disconnect,
    Terminate,
    #[cfg(unix)]
    Interrupt,
}

enum ConsoleInput {
    None,
    BeforePairing,
    AfterApproval,
}

#[tokio::test(flavor = "multi_thread")]
async fn consoles_have_independent_owners_and_print_the_approved_handoff() {
    check_console_ownership(ConsoleStop::Disconnect, ConsoleInput::None).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn terminating_a_console_stops_only_its_owned_bridge() {
    check_console_ownership(ConsoleStop::Terminate, ConsoleInput::None).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn enter_and_input_eof_do_not_stop_a_waiting_console() {
    check_console_ownership(ConsoleStop::Disconnect, ConsoleInput::BeforePairing).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pasted_lines_and_input_eof_do_not_stop_an_approved_console() {
    check_console_ownership(ConsoleStop::Disconnect, ConsoleInput::AfterApproval).await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn interrupting_a_console_stops_only_its_owned_bridge() {
    check_console_ownership(ConsoleStop::Interrupt, ConsoleInput::None).await;
}

fn send_console_input(console: &mut Console) {
    let mut input = console.child.stdin.take().unwrap();
    input.write_all(b"\nsynthetic console input\r\n\n").unwrap();
    // Close only the public input. The daemon's private ownership pipe must stay open.
}

fn disconnect(cache: &std::path::Path, session_id: &str) {
    let result = Command::new(env!("CARGO_BIN_EXE_gifgun-agent"))
        .args(["--session", session_id, "disconnect"])
        .env("GIFGUN_AGENT_CACHE_DIR", cache)
        .output()
        .unwrap();
    assert!(result.status.success());
}

async fn check_console_ownership(stop: ConsoleStop, input: ConsoleInput) {
    let directory = tempdir().unwrap();
    let mut first = Console::start(directory.path());
    if matches!(input, ConsoleInput::BeforePairing) {
        send_console_input(&mut first);
    }
    let mut second = Console::start(directory.path());
    let first_id = first.line("Workspace: ");
    let second_id = second.line("Workspace: ");
    assert_ne!(first_id, second_id);
    let address = url::Url::parse(&first.line("Editor: ")).unwrap();
    let port = address
        .query_pairs()
        .find(|(key, _)| key == "agentPort")
        .unwrap()
        .1
        .parse::<u16>()
        .unwrap();
    first.line("Waiting for the editor.");
    assert!(
        !first
            .lines
            .try_iter()
            .any(|line| line.contains("AI instruction"))
    );
    let contract = NativeContract::load_embedded().unwrap();
    let origin = address.origin().ascii_serialization();
    let client = Client::new();
    let expiry = SessionStore::at(directory.path())
        .read_session(Some(&first_id))
        .unwrap()
        .expires_at;
    let response = client.post(format!("http://127.0.0.1:{port}/v1/launch/{first_id}/{first_id}/pair"))
        .header("Origin", &origin).json(&json!({"version": 1, "port": port, "pairingToken": PAIR, "expectedOrigin": origin, "expiresAt": expiry, "compatibility": contract.compatibility()})).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let approved: Value = client
        .post(format!("http://127.0.0.1:{port}/v1/pair/approve"))
        .header("Origin", &origin)
        .bearer_auth(PAIR)
        .json(&json!({"compatibility": contract.compatibility()}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let stream = client
        .get(format!("http://127.0.0.1:{port}/v1/browser/stream"))
        .header("Origin", &origin)
        .bearer_auth(approved["browserToken"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    first.line("--- AI instruction ---");
    let mut output = String::new();
    loop {
        let line = first.lines.recv().unwrap();
        if line == "--- End of AI instruction ---" {
            break;
        }
        output.push_str(&line);
        output.push('\n');
    }
    assert_eq!(
        output.trim(),
        gifgun_agent::handoff::instruction(&first_id).unwrap()
    );
    assert!(!output.contains(&second_id));
    if matches!(input, ConsoleInput::AfterApproval) {
        send_console_input(&mut first);
        let handoff = client
            .get(format!("http://127.0.0.1:{port}/v1/browser/handoff"))
            .header("Origin", &origin)
            .bearer_auth(approved["browserToken"].as_str().unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(handoff.status(), StatusCode::OK);
    }
    let store = SessionStore::at(directory.path());
    match stop {
        ConsoleStop::Terminate => {
            first.child.kill().unwrap();
            first.child.wait().unwrap();
            // The daemon closes the authenticated stream when the ownership pipe closes.
            stream.bytes().await.unwrap();
        }
        ConsoleStop::Disconnect => {
            disconnect(directory.path(), &first_id);
            assert!(first.child.wait().unwrap().success());
            assert!(!store.session_path(&first_id).exists());
            drop(stream);
        }
        #[cfg(unix)]
        ConsoleStop::Interrupt => {
            // Keep the owned child unreaped so its PID cannot be reused before the signal.
            assert!(first.child.try_wait().unwrap().is_none());
            assert!(
                Command::new("/bin/kill")
                    .arg("-INT")
                    .arg(first.child.id().to_string())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .unwrap()
                    .success()
            );
            assert!(first.child.wait().unwrap().success());
            assert!(!store.session_path(&first_id).exists());
            stream.bytes().await.unwrap();
        }
    }
    drop(first);
    let status = Command::new(env!("CARGO_BIN_EXE_gifgun-agent"))
        .args(["--session", &second_id, "status"])
        .env("GIFGUN_AGENT_CACHE_DIR", directory.path())
        .output()
        .unwrap();
    assert!(status.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&status.stdout).unwrap()["sessionId"],
        second_id
    );
    disconnect(directory.path(), &second_id);
    assert!(second.child.wait().unwrap().success());
    drop(second);
    assert!(!store.session_path(&second_id).exists());
}
