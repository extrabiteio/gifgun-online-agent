use std::io::Write;
use std::net::TcpListener;
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use gifgun_agent::contract::{NativeContract, PairingEnvelope};
use gifgun_agent::session::Secret;
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

    let help = run_cli(&["project", "--help"], directory.path(), "");
    assert!(help.status.success());
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("open"));
    assert!(help.contains("save"));

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

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
