use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::TryStreamExt;
use gifgun_agent::contract::{NativeContract, PairingEnvelope};
use gifgun_agent::server::{BridgeServerOptions, GifGunBridgeServer};
use gifgun_agent::session::Secret;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::io::StreamReader;

const ORIGIN: &str = "https://gifgun.test";
const PAIRING_TOKEN: &str = "pairing-token-that-is-long-enough-1234567890";
const AGENT_TOKEN: &str = "agent-token-that-is-long-enough-123456789012";

async fn start() -> GifGunBridgeServer {
    let contract = NativeContract::load_embedded().unwrap();
    GifGunBridgeServer::start(BridgeServerOptions {
        envelope: PairingEnvelope {
            version: 1,
            port: 43_115,
            pairing_token: Secret::new(PAIRING_TOKEN.to_owned()),
            expected_origin: ORIGIN.to_owned(),
            expires_at: u64::MAX,
            compatibility: contract.compatibility(),
        },
        agent_token: Secret::new(AGENT_TOKEN.to_owned()),
        session_id: "session-test".to_owned(),
        port: 0,
        command_timeout: Duration::from_millis(250),
        session_expires_at: u64::MAX,
    })
    .await
    .unwrap()
}

fn browser_request(
    client: &Client,
    method: reqwest::Method,
    url: String,
    token: &str,
) -> reqwest::RequestBuilder {
    client
        .request(method, url)
        .header("Origin", ORIGIN)
        .bearer_auth(token)
}

async fn approve(server: &GifGunBridgeServer, client: &Client) -> String {
    let status = browser_request(
        client,
        reqwest::Method::GET,
        server.url("/v1/pair/status"),
        PAIRING_TOKEN,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(status.status(), StatusCode::OK);

    let contract = NativeContract::load_embedded().unwrap();
    let response = browser_request(
        client,
        reqwest::Method::POST,
        server.url("/v1/pair/approve"),
        PAIRING_TOKEN,
    )
    .json(&json!({ "compatibility": contract.compatibility() }))
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.json::<Value>().await.unwrap()["browserToken"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn binds_loopback_and_requires_exact_browser_origin_and_role_tokens() {
    let server = start().await;
    assert!(server.address().ip().is_loopback());
    assert!(server.address().is_ipv4());
    let client = Client::new();

    let missing_origin = client
        .get(server.url("/v1/pair/status"))
        .bearer_auth(PAIRING_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(missing_origin.status(), StatusCode::FORBIDDEN);

    let wrong_token = browser_request(
        &client,
        reqwest::Method::GET,
        server.url("/v1/pair/status"),
        "wrong-token",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(wrong_token.status(), StatusCode::UNAUTHORIZED);

    let status = browser_request(
        &client,
        reqwest::Method::GET,
        server.url("/v1/pair/status"),
        PAIRING_TOKEN,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(status.status(), StatusCode::OK);
    assert_eq!(status.headers()["access-control-allow-origin"], ORIGIN);
    assert_eq!(
        status.json::<Value>().await.unwrap()["status"],
        "approval_pending"
    );

    let preflight = client
        .request(reqwest::Method::OPTIONS, server.url("/v1/pair/status"))
        .header("Origin", ORIGIN)
        .header("Access-Control-Request-Method", "GET")
        .header("Access-Control-Request-Headers", "Authorization")
        .header("Access-Control-Request-Private-Network", "true")
        .send()
        .await
        .unwrap();
    assert_eq!(preflight.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        preflight.headers()["access-control-allow-private-network"],
        "true"
    );
    let forbidden_preflight = client
        .request(reqwest::Method::OPTIONS, server.url("/v1/pair/status"))
        .header("Origin", ORIGIN)
        .header("Access-Control-Request-Method", "DELETE")
        .header("Access-Control-Request-Headers", "X-Arbitrary")
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden_preflight.status(), StatusCode::FORBIDDEN);

    server.close().await.unwrap();
}

#[tokio::test]
async fn approves_streams_one_command_and_returns_the_matching_browser_result() {
    let server = start().await;
    let client = Client::new();
    let browser_token = approve(&server, &client).await;
    let contract = NativeContract::load_embedded().unwrap();
    let reused_pairing_token = browser_request(
        &client,
        reqwest::Method::POST,
        server.url("/v1/pair/approve"),
        PAIRING_TOKEN,
    )
    .json(&json!({"compatibility": contract.compatibility()}))
    .send()
    .await
    .unwrap();
    assert_eq!(reused_pairing_token.status(), StatusCode::UNAUTHORIZED);

    let response = browser_request(
        &client,
        reqwest::Method::GET,
        server.url("/v1/browser/stream"),
        &browser_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let stream = response.bytes_stream().map_err(std::io::Error::other);
    let mut lines = BufReader::new(StreamReader::new(stream)).lines();
    assert_eq!(
        serde_json::from_str::<Value>(&lines.next_line().await.unwrap().unwrap()).unwrap()["type"],
        "connected"
    );

    let command = json!({
        "protocol": contract.protocol(),
        "contractDigest": contract.digest(),
        "requestId": "command-1",
        "capabilityId": "editor.set_mode",
        "input": {"mode": "advanced"}
    });
    let agent_client = client.clone();
    let command_url = server.url("/v1/agent/command");
    let command_for_request = command.clone();
    let request = tokio::spawn(async move {
        agent_client
            .post(command_url)
            .bearer_auth(AGENT_TOKEN)
            .json(&command_for_request)
            .send()
            .await
            .unwrap()
    });

    let streamed =
        serde_json::from_str::<Value>(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(streamed["type"], "command");
    assert_eq!(streamed["command"], command);

    let in_flight_duplicate = client
        .post(server.url("/v1/agent/command"))
        .bearer_auth(AGENT_TOKEN)
        .json(&command)
        .send()
        .await
        .unwrap();
    assert_eq!(in_flight_duplicate.status(), StatusCode::CONFLICT);

    let result = json!({
        "ok": true,
        "requestId": "command-1",
        "revision": 4,
        "result": {"accepted": true}
    });
    let accepted = browser_request(
        &client,
        reqwest::Method::POST,
        server.url("/v1/browser/result"),
        &browser_token,
    )
    .json(&result)
    .send()
    .await
    .unwrap();
    assert_eq!(accepted.status(), StatusCode::OK);

    let response = request.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.json::<Value>().await.unwrap(), result);

    let duplicate = client
        .post(server.url("/v1/agent/command"))
        .bearer_auth(AGENT_TOKEN)
        .json(&command)
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate.status(), StatusCode::OK);
    assert_eq!(duplicate.json::<Value>().await.unwrap(), result);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), lines.next_line())
            .await
            .is_err()
    );
    server.close().await.unwrap();
}

#[tokio::test]
async fn reconnects_the_browser_stream_with_the_same_approved_credential() {
    let server = start().await;
    let client = Client::new();
    let browser_token = approve(&server, &client).await;

    for _ in 0..2 {
        let response = browser_request(
            &client,
            reqwest::Method::GET,
            server.url("/v1/browser/stream"),
            &browser_token,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let stream = response.bytes_stream().map_err(std::io::Error::other);
        let mut lines = BufReader::new(StreamReader::new(stream)).lines();
        let connected =
            serde_json::from_str::<Value>(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(connected["type"], "connected");
    }

    server.close().await.unwrap();
}

#[tokio::test]
async fn rejects_incompatible_approval_invalid_commands_and_unanswered_timeouts() {
    let mut server = start().await;
    let client = Client::new();
    let browser_token = approve(&server, &client).await;
    let stream = browser_request(
        &client,
        reqwest::Method::GET,
        server.url("/v1/browser/stream"),
        &browser_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(stream.status(), StatusCode::OK);

    let invalid = client
        .post(server.url("/v1/agent/command"))
        .bearer_auth(AGENT_TOKEN)
        .json(&json!({"arbitraryScript": "alert(document.cookie)"}))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

    let wrong_content_type = client
        .post(server.url("/v1/agent/command"))
        .bearer_auth(AGENT_TOKEN)
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_content_type.status(), StatusCode::BAD_REQUEST);

    let oversized = client
        .post(server.url("/v1/agent/command"))
        .bearer_auth(AGENT_TOKEN)
        .header("Content-Type", "application/json")
        .body(format!("{{\"padding\":\"{}\"}}", "x".repeat(256 * 1024)))
        .send()
        .await
        .unwrap();
    assert_eq!(oversized.status(), StatusCode::BAD_REQUEST);

    let contract = NativeContract::load_embedded().unwrap();
    let timeout = client
        .post(server.url("/v1/agent/command"))
        .bearer_auth(AGENT_TOKEN)
        .json(&json!({
            "protocol": contract.protocol(),
            "contractDigest": contract.digest(),
            "requestId": "unanswered",
            "capabilityId": "editor.get_state",
            "input": {}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(timeout.status(), StatusCode::GATEWAY_TIMEOUT);
    server.close().await.unwrap();

    let mut stale_options = BridgeServerOptions::for_test(0, ORIGIN, PAIRING_TOKEN, AGENT_TOKEN);
    stale_options.envelope.compatibility.contract_digest = "stale-contract".to_owned();
    stale_options.envelope.expires_at = u64::MAX;
    server = GifGunBridgeServer::start(stale_options).await.unwrap();
    let status = browser_request(
        &client,
        reqwest::Method::GET,
        server.url("/v1/pair/status"),
        PAIRING_TOKEN,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(status.status(), StatusCode::OK);
    assert_eq!(
        status.json::<Value>().await.unwrap()["status"],
        "incompatible"
    );
    let rejected_approval = browser_request(
        &client,
        reqwest::Method::POST,
        server.url("/v1/pair/approve"),
        PAIRING_TOKEN,
    )
    .json(&json!({"compatibility": NativeContract::load_embedded().unwrap().compatibility()}))
    .send()
    .await
    .unwrap();
    assert_eq!(rejected_approval.status(), StatusCode::CONFLICT);
    let rejected_transfer = client
        .post(server.url("/v1/agent/transfers/upload"))
        .bearer_auth(AGENT_TOKEN)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected_transfer.status(), StatusCode::CONFLICT);
    server.close().await.unwrap();
}

#[tokio::test]
async fn pairing_and_established_sessions_expire_and_stop_the_bridge() {
    let mut pairing_options = BridgeServerOptions::for_test(0, ORIGIN, PAIRING_TOKEN, AGENT_TOKEN);
    pairing_options.envelope.expires_at = now_millis() + 250;
    let pairing_server = GifGunBridgeServer::start(pairing_options).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), pairing_server.wait())
        .await
        .unwrap()
        .unwrap();

    let mut session_options = BridgeServerOptions::for_test(0, ORIGIN, PAIRING_TOKEN, AGENT_TOKEN);
    session_options.envelope.expires_at = u64::MAX;
    session_options.session_expires_at = now_millis() + 500;
    let session_server = GifGunBridgeServer::start(session_options).await.unwrap();
    let client = Client::new();
    approve(&session_server, &client).await;
    tokio::time::timeout(Duration::from_secs(1), session_server.wait())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn streams_authenticated_upload_and_output_reservations_without_paths() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.mp4");
    tokio::fs::write(&source, b"private-video-sentinel")
        .await
        .unwrap();
    let output = directory.path().join("result.gif");
    let server = start().await;
    let client = Client::new();
    let browser_token = approve(&server, &client).await;

    let upload = client
        .post(server.url("/v1/agent/transfers/upload"))
        .bearer_auth(AGENT_TOKEN)
        .json(&json!({"path": source}))
        .send()
        .await
        .unwrap();
    assert_eq!(upload.status(), StatusCode::OK);
    let upload = upload.json::<Value>().await.unwrap();
    assert!(upload.get("path").is_none());
    let upload_id = upload["transferId"].as_str().unwrap();
    let downloaded = browser_request(
        &client,
        reqwest::Method::GET,
        server.url(&format!("/v1/browser/transfers/{upload_id}")),
        &browser_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(downloaded.status(), StatusCode::OK);
    assert_eq!(
        downloaded.bytes().await.unwrap(),
        &b"private-video-sentinel"[..]
    );

    let reservation = client
        .post(server.url("/v1/agent/transfers/output"))
        .bearer_auth(AGENT_TOKEN)
        .json(&json!({"path": output, "overwrite": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(reservation.status(), StatusCode::OK);
    let reservation = reservation.json::<Value>().await.unwrap();
    assert!(reservation.get("path").is_none());
    let output_id = reservation["transferId"].as_str().unwrap();
    let bytes = b"GIF89a";
    let saved = browser_request(
        &client,
        reqwest::Method::POST,
        server.url(&format!("/v1/browser/transfers/{output_id}")),
        &browser_token,
    )
    .header("Content-Type", "image/gif")
    .header("Content-Length", bytes.len())
    .body(bytes.to_vec())
    .send()
    .await
    .unwrap();
    assert_eq!(saved.status(), StatusCode::OK);
    assert_eq!(
        saved.json::<Value>().await.unwrap(),
        json!({"size": bytes.len()})
    );
    assert_eq!(tokio::fs::read(&output).await.unwrap(), bytes);

    server.close().await.unwrap();
}

#[tokio::test]
async fn explicit_disconnect_fails_pending_work_and_stops_the_bridge() {
    let server = start().await;
    let client = Client::new();
    let browser_token = approve(&server, &client).await;
    let stream = browser_request(
        &client,
        reqwest::Method::GET,
        server.url("/v1/browser/stream"),
        &browser_token,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(stream.status(), StatusCode::OK);

    let contract = NativeContract::load_embedded().unwrap();
    let agent_client = client.clone();
    let command_url = server.url("/v1/agent/command");
    let pending = tokio::spawn(async move {
        agent_client
            .post(command_url)
            .bearer_auth(AGENT_TOKEN)
            .json(&json!({
                "protocol": contract.protocol(),
                "contractDigest": contract.digest(),
                "requestId": "pending-disconnect",
                "capabilityId": "editor.get_state",
                "input": {}
            }))
            .send()
            .await
            .unwrap()
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let response = browser_request(
        &client,
        reqwest::Method::POST,
        server.url("/v1/browser/disconnect"),
        &browser_token,
    )
    .json(&json!({}))
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(pending.await.unwrap().status(), StatusCode::CONFLICT);
    server.wait().await.unwrap();
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
