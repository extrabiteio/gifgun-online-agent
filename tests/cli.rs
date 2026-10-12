use std::io::Write;
use std::net::TcpListener;
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use gifgun_agent::contract::{NativeContract, PairingEnvelope};
use gifgun_agent::session::{AgentSessionRecord, Secret, SessionStore};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use tempfile::tempdir;

const ORIGIN: &str = "https://gifgun.test";
const PAIRING_TOKEN: &str = "synthetic-pairing-token-12345678901234567890";

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_gifgun-agent")
}

fn run_cli(args: &[&str], cache: &std::path::Path, input: &str) -> Output {
    let mut child = Command::new(binary())
        .args(args)
        .env("GIFGUN_AGENT_CACHE_DIR", cache)
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("NO_PROXY", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn reports_version_and_rejects_malformed_pairing_without_echoing_input() {
    let directory = tempdir().unwrap();
    let version = run_cli(&["--version"], directory.path(), "");
    assert!(version.status.success());
    let version = String::from_utf8_lossy(&version.stdout);
    assert!(version.contains(concat!("gifgun-agent ", env!("CARGO_PKG_VERSION"))));
    assert!(version.contains(NativeContract::load_embedded().unwrap().digest()));

    let help = run_cli(&["--help"], directory.path(), "");
    assert!(help.status.success());
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("capabilities --list"));
    assert!(help.contains("state --summary"));
    assert!(help.contains("Never inspect GifGun Online or GifGun Agent source code."));

    let capabilities_help = run_cli(&["capabilities", "--help"], directory.path(), "");
    assert!(capabilities_help.status.success());
    let capabilities_help = String::from_utf8_lossy(&capabilities_help.stdout);
    assert!(capabilities_help.contains("--list"));
    assert!(capabilities_help.contains("--id <CAPABILITY>"));
    assert!(capabilities_help.contains("--full"));

    let state_help = run_cli(&["state", "--help"], directory.path(), "");
    assert!(state_help.status.success());
    let state_help = String::from_utf8_lossy(&state_help.stdout);
    assert!(state_help.contains("--summary"));
    assert!(state_help.contains("--full"));

    let call_help = run_cli(&["call", "--help"], directory.path(), "");
    assert!(call_help.status.success());
    let call_help = String::from_utf8_lossy(&call_help.stdout);
    assert!(call_help.contains("Read one capability input JSON value from stdin"));
    assert!(call_help.contains("Chain"));

    let render_help = run_cli(&["render", "--help"], directory.path(), "");
    assert!(render_help.status.success());
    let render_help = String::from_utf8_lossy(&render_help.stdout);
    assert!(render_help.contains("Do not start another render"));

    let project_help = run_cli(&["project", "--help"], directory.path(), "");
    assert!(project_help.status.success());
    let project_help = String::from_utf8_lossy(&project_help.stdout);
    assert!(project_help.contains("open"));
    assert!(project_help.contains("save"));

    let malformed = run_cli(&["pair"], directory.path(), "private-malformed-token\n");
    assert!(!malformed.status.success());
    let error = String::from_utf8_lossy(&malformed.stderr);
    assert!(error.contains("pairing instruction is malformed"));
    assert!(!error.contains("private-malformed-token"));

    let contract = NativeContract::load_embedded().unwrap();
    let mut compatibility = contract.compatibility();
    compatibility.contract_digest = "stale-contract".to_owned();
    let envelope = PairingEnvelope {
        version: 1,
        port: 43_115,
        pairing_token: Secret::new(PAIRING_TOKEN.to_owned()),
        expected_origin: ORIGIN.to_owned(),
        expires_at: now_millis() + 60_000,
        compatibility,
    };
    let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&envelope).unwrap());
    let incompatible = run_cli(
        &["pair"],
        directory.path(),
        &format!("Pairing envelope: {encoded}\n"),
    );
    assert!(!incompatible.status.success());
    assert!(
        String::from_utf8_lossy(&incompatible.stderr)
            .contains("incompatible with the open GifGun tab")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn real_cli_pairs_tracks_status_and_disconnects_native_daemon() {
    let directory = tempdir().unwrap();
    let port = {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.local_addr().unwrap().port()
    };
    let contract = NativeContract::load_embedded().unwrap();
    let envelope = PairingEnvelope {
        version: 1,
        port,
        pairing_token: Secret::new(PAIRING_TOKEN.to_owned()),
        expected_origin: ORIGIN.to_owned(),
        expires_at: now_millis() + 60_000,
        compatibility: contract.compatibility(),
    };
    let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&envelope).unwrap());
    let instruction = format!(
        "Download the required native agent.\nPass this complete instruction to pair on stdin.\nPairing envelope: {encoded}\n"
    );

    let paired = run_cli(&["pair"], directory.path(), &instruction);
    assert!(
        paired.status.success(),
        "{}",
        String::from_utf8_lossy(&paired.stderr)
    );
    let paired_json: Value = serde_json::from_slice(&paired.stdout).unwrap();
    assert_eq!(paired_json["port"], port);
    let output = String::from_utf8_lossy(&paired.stdout);
    assert!(!output.contains(PAIRING_TOKEN));
    assert!(!output.contains(directory.path().to_string_lossy().as_ref()));

    let client = Client::new();
    let status = client
        .get(format!("http://127.0.0.1:{port}/v1/pair/status"))
        .header("Origin", ORIGIN)
        .bearer_auth(PAIRING_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK);

    let approved = client
        .post(format!("http://127.0.0.1:{port}/v1/pair/approve"))
        .header("Origin", ORIGIN)
        .bearer_auth(PAIRING_TOKEN)
        .json(&json!({"compatibility": contract.compatibility()}))
        .send()
        .await
        .unwrap();
    assert_eq!(approved.status(), StatusCode::OK);

    let status = run_cli(&["status"], directory.path(), "");
    assert!(status.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&status.stdout).unwrap()["status"],
        "connected"
    );

    let disconnected = run_cli(&["disconnect"], directory.path(), "");
    assert!(disconnected.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&disconnected.stdout).unwrap()["status"],
        "disconnected"
    );
}

#[test]
fn accepts_a_session_identifier_that_starts_with_a_hyphen() {
    let directory = tempdir().unwrap();
    let store = SessionStore::at(directory.path());
    let record = AgentSessionRecord::new(
        "-synthetic-session".to_owned(),
        0,
        "synthetic-agent-token".to_owned(),
    );
    store.write_session(&record).unwrap();
    let result = run_cli(
        &["--session", &record.session_id, "disconnect"],
        directory.path(),
        "",
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap()["sessionId"],
        record.session_id
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_managed_sessions_use_owned_ports_and_explicit_targets() {
    let directory = tempdir().unwrap();
    let (first, second) = std::thread::scope(|scope| {
        let first = scope.spawn(|| run_cli(&["open", "--no-browser"], directory.path(), ""));
        let second = scope.spawn(|| run_cli(&["open", "--no-browser"], directory.path(), ""));
        (first.join().unwrap(), second.join().unwrap())
    });
    let values = [first, second].map(|output| {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&output.stdout)
                .contains(directory.path().to_string_lossy().as_ref())
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    });
    struct Cleanup<'a> {
        cache: &'a std::path::Path,
        ids: Vec<String>,
    }
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            for id in &self.ids {
                let _ = run_cli(&["--session", id, "disconnect"], self.cache, "");
            }
        }
    }
    let cleanup = Cleanup {
        cache: directory.path(),
        ids: values
            .iter()
            .map(|value| value["sessionId"].as_str().unwrap().to_owned())
            .collect(),
    };
    assert_ne!(values[0]["sessionId"], values[1]["sessionId"]);
    assert_ne!(values[0]["port"], values[1]["port"]);
    assert!(!directory.path().join("current").exists());
    let default = run_cli(&["status"], directory.path(), "");
    assert!(!default.status.success());
    assert!(String::from_utf8_lossy(&default.stderr).contains("--session"));
    let client = Client::new();
    let contract = NativeContract::load_embedded().unwrap();
    for value in &values {
        let id = value["sessionId"].as_str().unwrap();
        let workspace = value["workspaceId"].as_str().unwrap();
        let port = value["port"].as_u64().unwrap();
        let url = url::Url::parse(value["editorUrl"].as_str().unwrap()).unwrap();
        let origin = url.origin().ascii_serialization();
        let paired = client.post(format!("http://127.0.0.1:{port}/v1/launch/{id}/{workspace}/pair")).header("Origin", &origin)
            .json(&json!({"version": 1, "port": port, "pairingToken": PAIRING_TOKEN, "expectedOrigin": origin, "expiresAt": now_millis() + 60_000, "compatibility": contract.compatibility()})).send().await.unwrap();
        assert_eq!(paired.status(), StatusCode::OK);
        let approved = client
            .post(format!("http://127.0.0.1:{port}/v1/pair/approve"))
            .header("Origin", &origin)
            .bearer_auth(PAIRING_TOKEN)
            .json(&json!({"compatibility": contract.compatibility()}))
            .send()
            .await
            .unwrap();
        assert_eq!(approved.status(), StatusCode::OK);
        let status = run_cli(&["--session", id, "status"], directory.path(), "");
        assert!(status.status.success());
        let status: Value = serde_json::from_slice(&status.stdout).unwrap();
        assert_eq!(status["sessionId"], id);
        assert_eq!(status["status"], "connected");
    }
    let stopped = run_cli(
        &["--session", &cleanup.ids[0], "disconnect"],
        directory.path(),
        "",
    );
    assert!(
        stopped.status.success(),
        "{}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    let other = run_cli(
        &["--session", &cleanup.ids[1], "status"],
        directory.path(),
        "",
    );
    assert!(other.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&other.stdout).unwrap()["sessionId"],
        cleanup.ids[1]
    );
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
