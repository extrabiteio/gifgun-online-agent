use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Path, State};
use axum::http::header::{
    ACCESS_CONTROL_REQUEST_HEADERS, ACCESS_CONTROL_REQUEST_METHOD, AUTHORIZATION, CACHE_CONTROL,
    CONTENT_LENGTH, CONTENT_TYPE, ORIGIN, VARY,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use bytes::Bytes;
use futures_util::{TryStreamExt, stream};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_util::io::{ReaderStream, StreamReader};
use url::Url;

use crate::contract::{Compatibility, NativeContract, PairingEnvelope};
use crate::error::{AgentError, AgentResult};
use crate::session::{Secret, random_secret, secrets_equal};
use crate::transfers::TransferManager;

const MAX_JSON_BYTES: usize = 256 * 1024;
const MAX_PENDING_COMMANDS: usize = 1;
const MAX_RESULT_CACHE: usize = 200;
const STREAM_CAPACITY: usize = 64;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum BridgeStatus {
    Waiting,
    ApprovalPending,
    Connected,
    Rejected,
    Expired,
    Incompatible,
}

#[derive(Clone)]
struct BrowserStream {
    id: String,
    sender: mpsc::Sender<Result<Bytes, Infallible>>,
}

struct PendingCommand {
    sender: oneshot::Sender<Result<Value, String>>,
}

struct BridgeInner {
    status: BridgeStatus,
    pairing_token: Option<Secret>,
    browser_token: Option<Secret>,
    browser_stream: Option<BrowserStream>,
    pending: HashMap<String, PendingCommand>,
    result_cache: HashMap<String, Value>,
    result_order: VecDeque<String>,
}

struct BridgeState {
    contract: Arc<NativeContract>,
    expected_origin: String,
    compatibility: Compatibility,
    agent_token: Secret,
    session_id: String,
    pairing_expires_at: u64,
    session_expires_at: u64,
    command_timeout: Duration,
    inner: Mutex<BridgeInner>,
    transfers: TransferManager,
    shutdown: watch::Sender<bool>,
}

impl BridgeState {
    async fn shutdown(&self, status: BridgeStatus, message: &str) {
        let mut inner = self.inner.lock().await;
        inner.status = status;
        inner.pairing_token = None;
        inner.browser_token = None;
        inner.browser_stream = None;
        for (_, pending) in inner.pending.drain() {
            let _ = pending.sender.send(Err(message.to_owned()));
        }
        self.transfers.clear();
        drop(inner);
        let _ = self.shutdown.send(true);
    }

    async fn send_stream_record(&self, record: Value) -> Result<(), ()> {
        let stream = self.inner.lock().await.browser_stream.clone().ok_or(())?;
        let line = Bytes::from(format!("{record}\n"));
        if stream.sender.try_send(Ok(line)).is_ok() {
            return Ok(());
        }
        let mut inner = self.inner.lock().await;
        if inner
            .browser_stream
            .as_ref()
            .is_some_and(|known| known.id == stream.id)
        {
            inner.browser_stream = None;
        }
        Err(())
    }

    async fn browser_connected(&self) -> bool {
        self.inner
            .lock()
            .await
            .browser_stream
            .as_ref()
            .is_some_and(|stream| !stream.sender.is_closed())
    }
}

pub struct BridgeServerOptions {
    pub envelope: PairingEnvelope,
    pub agent_token: Secret,
    pub session_id: String,
    pub port: u16,
    pub command_timeout: Duration,
    pub session_expires_at: u64,
}

impl BridgeServerOptions {
    #[doc(hidden)]
    pub fn for_test(port: u16, origin: &str, pairing_token: &str, agent_token: &str) -> Self {
        let contract = NativeContract::load_embedded().expect("embedded contract");
        Self {
            envelope: PairingEnvelope {
                version: 1,
                port: if port == 0 { 43_115 } else { port },
                pairing_token: Secret::new(pairing_token.to_owned()),
                expected_origin: origin.to_owned(),
                expires_at: now_millis().saturating_add(60_000),
                compatibility: contract.compatibility(),
            },
            agent_token: Secret::new(agent_token.to_owned()),
            session_id: "session-test".to_owned(),
            port,
            command_timeout: Duration::from_millis(250),
            session_expires_at: u64::MAX,
        }
    }
}

#[derive(Clone)]
pub struct BridgeShutdown {
    state: Arc<BridgeState>,
}

impl BridgeShutdown {
    pub async fn close(&self) {
        self.state
            .shutdown(
                BridgeStatus::Rejected,
                "Bridge stopped before the browser responded.",
            )
            .await;
    }
}

pub struct GifGunBridgeServer {
    state: Arc<BridgeState>,
    address: SocketAddr,
    task: JoinHandle<io::Result<()>>,
}

impl GifGunBridgeServer {
    pub async fn start(options: BridgeServerOptions) -> AgentResult<Self> {
        let contract = Arc::new(NativeContract::load_embedded()?);
        let envelope_value = serde_json::to_value(&options.envelope)
            .map_err(|error| AgentError::State(format!("pairing envelope is invalid: {error}")))?;
        contract.validate_pairing(&envelope_value).map_err(|_| {
            AgentError::State("pairing envelope does not match the contract".into())
        })?;
        let parsed_origin = Url::parse(&options.envelope.expected_origin)
            .map_err(|_| AgentError::State("expected browser origin is invalid".into()))?;
        if !matches!(parsed_origin.scheme(), "http" | "https")
            || parsed_origin.origin().ascii_serialization() != options.envelope.expected_origin
        {
            return Err(AgentError::State(
                "expected browser origin must be an HTTP origin without a path".into(),
            ));
        }
        if options.envelope.expires_at <= now_millis() {
            return Err(AgentError::State("pairing instruction has expired".into()));
        }
        if options.session_expires_at <= now_millis() {
            return Err(AgentError::State("agent session has expired".into()));
        }
        let status = if contract
            .check_compatibility(&options.envelope.compatibility)
            .is_ok()
        {
            BridgeStatus::Waiting
        } else {
            BridgeStatus::Incompatible
        };
        let listener = TcpListener::bind(("127.0.0.1", options.port)).await?;
        let address = listener.local_addr()?;
        if !address.ip().is_ipv4() || !address.ip().is_loopback() {
            return Err(AgentError::State(
                "local bridge did not bind to IPv4 loopback".into(),
            ));
        }
        let (shutdown, mut shutdown_receiver) = watch::channel(false);
        let state = Arc::new(BridgeState {
            contract: Arc::clone(&contract),
            expected_origin: options.envelope.expected_origin,
            compatibility: contract.compatibility(),
            agent_token: options.agent_token,
            session_id: options.session_id,
            pairing_expires_at: options.envelope.expires_at,
            session_expires_at: options.session_expires_at,
            command_timeout: if options.command_timeout.is_zero() {
                DEFAULT_COMMAND_TIMEOUT
            } else {
                options.command_timeout
            },
            inner: Mutex::new(BridgeInner {
                status,
                pairing_token: Some(options.envelope.pairing_token),
                browser_token: None,
                browser_stream: None,
                pending: HashMap::new(),
                result_cache: HashMap::new(),
                result_order: VecDeque::new(),
            }),
            transfers: TransferManager::default(),
            shutdown,
        });
        let router = router(Arc::clone(&state));
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    while !*shutdown_receiver.borrow() {
                        if shutdown_receiver.changed().await.is_err() {
                            break;
                        }
                    }
                })
                .await
        });
        spawn_heartbeat(Arc::clone(&state));
        spawn_expiry(Arc::clone(&state));
        Ok(Self {
            state,
            address,
            task,
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.address.port())
    }

    pub fn shutdown_handle(&self) -> BridgeShutdown {
        BridgeShutdown {
            state: Arc::clone(&self.state),
        }
    }

    pub async fn close(self) -> AgentResult<()> {
        self.state
            .shutdown(
                BridgeStatus::Rejected,
                "Bridge stopped before the browser responded.",
            )
            .await;
        self.join().await
    }

    pub async fn wait(self) -> AgentResult<()> {
        self.join().await
    }

    async fn join(self) -> AgentResult<()> {
        self.task
            .await
            .map_err(|error| AgentError::State(format!("bridge task failed: {error}")))??;
        Ok(())
    }
}

fn router(state: Arc<BridgeState>) -> Router {
    Router::new()
        .route(
            "/v1/pair/status",
            get(pair_status).options(browser_preflight),
        )
        .route(
            "/v1/pair/approve",
            post(pair_approve).options(browser_preflight),
        )
        .route(
            "/v1/pair/reject",
            post(pair_reject).options(browser_preflight),
        )
        .route(
            "/v1/browser/stream",
            get(browser_stream).options(browser_preflight),
        )
        .route(
            "/v1/browser/result",
            post(browser_result).options(browser_preflight),
        )
        .route(
            "/v1/browser/disconnect",
            post(browser_disconnect).options(browser_preflight),
        )
        .route(
            "/v1/browser/transfers/{transfer_id}",
            get(browser_upload)
                .post(browser_output)
                .options(browser_preflight),
        )
        .route("/v1/agent/status", get(agent_status))
        .route("/v1/agent/command", post(agent_command))
        .route("/v1/agent/transfers/upload", post(agent_upload_reservation))
        .route("/v1/agent/transfers/output", post(agent_output_reservation))
        .route("/v1/agent/disconnect", post(agent_disconnect))
        .with_state(state)
}

async fn browser_preflight(State(state): State<Arc<BridgeState>>, headers: HeaderMap) -> Response {
    if !has_browser_origin(&state, &headers) {
        return json_response(
            &state,
            StatusCode::FORBIDDEN,
            json!({"error": "origin_not_allowed"}),
            false,
        );
    }
    let method_allowed = headers
        .get(ACCESS_CONTROL_REQUEST_METHOD)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|value| matches!(value, "GET" | "POST" | "OPTIONS"));
    let headers_allowed = headers
        .get(ACCESS_CONTROL_REQUEST_HEADERS)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|value| {
            value.split(',').all(|header| {
                matches!(
                    header.trim().to_ascii_lowercase().as_str(),
                    "authorization" | "content-type"
                )
            })
        });
    if !method_allowed || !headers_allowed {
        return json_response(
            &state,
            StatusCode::FORBIDDEN,
            json!({"error": "preflight_not_allowed"}),
            true,
        );
    }
    empty_browser_response(&state, StatusCode::NO_CONTENT)
}

async fn pair_status(State(state): State<Arc<BridgeState>>, headers: HeaderMap) -> Response {
    if let Some(response) = authorize_browser_pairing(&state, &headers).await {
        return response;
    }
    let mut inner = state.inner.lock().await;
    if inner.status == BridgeStatus::Waiting {
        inner.status = BridgeStatus::ApprovalPending;
    }
    let status = inner.status;
    drop(inner);
    json_response(
        &state,
        StatusCode::OK,
        json!({
            "status": status,
            "candidate": {"sessionId": state.session_id, "name": "Local GifGun agent"},
            "compatibility": state.compatibility,
        }),
        true,
    )
}

async fn pair_approve(
    State(state): State<Arc<BridgeState>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Some(response) = authorize_browser_pairing(&state, &headers).await {
        return response;
    }
    if state.inner.lock().await.status == BridgeStatus::Incompatible {
        return json_response(
            &state,
            StatusCode::CONFLICT,
            json!({"error": "incompatible_contract"}),
            true,
        );
    }
    let value = match read_json(&headers, body).await {
        Ok(value) => value,
        Err(message) => return invalid_request(&state, message, true),
    };
    let compatibility = value
        .get("compatibility")
        .cloned()
        .and_then(|value| serde_json::from_value::<Compatibility>(value).ok());
    if compatibility
        .as_ref()
        .is_none_or(|value| state.contract.check_compatibility(value).is_err())
    {
        return json_response(
            &state,
            StatusCode::CONFLICT,
            json!({"error": "incompatible_contract"}),
            true,
        );
    }
    let browser_token = match random_secret(32) {
        Ok(value) => Secret::new(value),
        Err(_) => return invalid_request(&state, "browser credential unavailable", true),
    };
    let mut inner = state.inner.lock().await;
    if !has_token(&headers, inner.pairing_token.as_ref()) {
        return json_response(
            &state,
            StatusCode::UNAUTHORIZED,
            json!({"error": "invalid_pairing_token"}),
            true,
        );
    }
    inner.browser_token = Some(browser_token.clone());
    inner.pairing_token = None;
    inner.status = BridgeStatus::Connected;
    drop(inner);
    json_response(
        &state,
        StatusCode::OK,
        json!({
            "status": "connected",
            "sessionId": state.session_id,
            "browserToken": browser_token.expose(),
            "compatibility": state.compatibility,
        }),
        true,
    )
}

async fn pair_reject(State(state): State<Arc<BridgeState>>, headers: HeaderMap) -> Response {
    if let Some(response) = authorize_browser_pairing(&state, &headers).await {
        return response;
    }
    state
        .shutdown(BridgeStatus::Rejected, "Browser rejected the connection.")
        .await;
    json_response(&state, StatusCode::OK, json!({"status": "rejected"}), true)
}

async fn browser_stream(State(state): State<Arc<BridgeState>>, headers: HeaderMap) -> Response {
    if let Some(response) = authorize_browser_session(&state, &headers).await {
        return response;
    }
    let (sender, receiver) = mpsc::channel(STREAM_CAPACITY);
    let id = match random_secret(12) {
        Ok(value) => value,
        Err(_) => return invalid_request(&state, "browser stream unavailable", true),
    };
    let stream = BrowserStream {
        id,
        sender: sender.clone(),
    };
    state.inner.lock().await.browser_stream = Some(stream);
    let _ = sender
        .send(Ok(Bytes::from(format!(
            "{}\n",
            json!({"type": "connected", "at": now_millis()})
        ))))
        .await;
    let body_stream = stream::unfold(receiver, |mut receiver| async {
        receiver.recv().await.map(|item| (item, receiver))
    });
    stream_response(&state, Body::from_stream(body_stream))
}

async fn browser_result(
    State(state): State<Arc<BridgeState>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Some(response) = authorize_browser_session(&state, &headers).await {
        return response;
    }
    let value = match read_json(&headers, body).await {
        Ok(value) => value,
        Err(message) => return invalid_request(&state, message, true),
    };
    if state.contract.validate_result(&value).is_err() {
        return json_response(
            &state,
            StatusCode::BAD_REQUEST,
            json!({"error": "invalid_command_result"}),
            true,
        );
    }
    let Some(request_id) = value
        .get("requestId")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return json_response(
            &state,
            StatusCode::BAD_REQUEST,
            json!({"error": "invalid_command_result"}),
            true,
        );
    };
    let mut inner = state.inner.lock().await;
    let Some(pending) = inner.pending.remove(&request_id) else {
        return json_response(
            &state,
            StatusCode::CONFLICT,
            json!({"error": "command_not_pending"}),
            true,
        );
    };
    cache_result(&mut inner, request_id, value.clone());
    drop(inner);
    let _ = pending.sender.send(Ok(value));
    json_response(&state, StatusCode::OK, json!({"accepted": true}), true)
}

async fn browser_disconnect(State(state): State<Arc<BridgeState>>, headers: HeaderMap) -> Response {
    if let Some(response) = authorize_browser_session(&state, &headers).await {
        return response;
    }
    state
        .shutdown(BridgeStatus::Rejected, "Browser disconnected.")
        .await;
    json_response(&state, StatusCode::OK, json!({"status": "rejected"}), true)
}

async fn browser_upload(
    State(state): State<Arc<BridgeState>>,
    Path(transfer_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = authorize_browser_session(&state, &headers).await {
        return response;
    }
    let upload = match state.transfers.take_upload(&transfer_id).await {
        Ok(upload) => upload,
        Err(_) => return invalid_request(&state, "transfer unavailable", true),
    };
    let mut response = Response::new(Body::from_stream(ReaderStream::new(upload.file)));
    *response.status_mut() = StatusCode::OK;
    let response_headers = response.headers_mut();
    add_browser_headers(&state, response_headers);
    response_headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_str(&upload.metadata.content_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    response_headers.insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&upload.metadata.size.to_string()).expect("numeric header"),
    );
    response_headers.insert(
        HeaderName::from_static("x-gifgun-file-name"),
        HeaderValue::from_str(
            &utf8_percent_encode(&upload.metadata.name, NON_ALPHANUMERIC).to_string(),
        )
        .unwrap_or_else(|_| HeaderValue::from_static("local-media")),
    );
    response_headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response
}

async fn browser_output(
    State(state): State<Arc<BridgeState>>,
    Path(transfer_id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Some(response) = authorize_browser_session(&state, &headers).await {
        return response;
    }
    let expected_size = headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    let Some(expected_size) = expected_size else {
        return json_response(
            &state,
            StatusCode::LENGTH_REQUIRED,
            json!({"error": "content_length_required"}),
            true,
        );
    };
    let stream = body.into_data_stream().map_err(io::Error::other);
    let reader = StreamReader::new(stream);
    match state
        .transfers
        .write_output(&transfer_id, reader, expected_size)
        .await
    {
        Ok(result) => json_response(&state, StatusCode::OK, json!(result), true),
        Err(_) => invalid_request(&state, "output transfer failed", true),
    }
}

async fn agent_status(State(state): State<Arc<BridgeState>>, headers: HeaderMap) -> Response {
    if !has_agent_token(&state, &headers) {
        return json_response(
            &state,
            StatusCode::UNAUTHORIZED,
            json!({"error": "invalid_agent_session"}),
            false,
        );
    }
    let status = state.inner.lock().await.status;
    json_response(
        &state,
        StatusCode::OK,
        json!({
            "sessionId": state.session_id,
            "status": status,
            "browserConnected": state.browser_connected().await,
            "compatibility": state.compatibility,
        }),
        false,
    )
}

async fn agent_command(
    State(state): State<Arc<BridgeState>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if !has_agent_token(&state, &headers) {
        return json_response(
            &state,
            StatusCode::UNAUTHORIZED,
            json!({"error": "invalid_agent_session"}),
            false,
        );
    }
    let value = match read_json(&headers, body).await {
        Ok(value) => value,
        Err(message) => return invalid_request(&state, message, false),
    };
    if let Err(error) = state.contract.validate_command(&value) {
        return json_response(
            &state,
            StatusCode::BAD_REQUEST,
            json!({"error": {"code": error.code, "message": error.message}}),
            false,
        );
    }
    let request_id = value
        .get("requestId")
        .and_then(Value::as_str)
        .expect("validated request id")
        .to_owned();
    let receiver = {
        let mut inner = state.inner.lock().await;
        if let Some(cached) = inner.result_cache.get(&request_id) {
            return json_response(&state, StatusCode::OK, cached.clone(), false);
        }
        if inner.status != BridgeStatus::Connected
            || inner
                .browser_stream
                .as_ref()
                .is_none_or(|stream| stream.sender.is_closed())
        {
            return json_response(
                &state,
                StatusCode::CONFLICT,
                json!({"error": "browser_disconnected"}),
                false,
            );
        }
        if inner.pending.contains_key(&request_id) || inner.pending.len() >= MAX_PENDING_COMMANDS {
            return json_response(
                &state,
                StatusCode::CONFLICT,
                json!({"error": "command_already_pending"}),
                false,
            );
        }
        let (sender, receiver) = oneshot::channel();
        inner
            .pending
            .insert(request_id.clone(), PendingCommand { sender });
        receiver
    };
    if state
        .send_stream_record(json!({"type": "command", "command": value}))
        .await
        .is_err()
    {
        state.inner.lock().await.pending.remove(&request_id);
        return json_response(
            &state,
            StatusCode::CONFLICT,
            json!({"error": "browser_disconnected"}),
            false,
        );
    }
    match tokio::time::timeout(state.command_timeout, receiver).await {
        Ok(Ok(Ok(result))) => json_response(&state, StatusCode::OK, result, false),
        Ok(Ok(Err(message))) => json_response(
            &state,
            StatusCode::CONFLICT,
            json!({"error": "browser_disconnected", "message": message}),
            false,
        ),
        _ => {
            state.inner.lock().await.pending.remove(&request_id);
            json_response(
                &state,
                StatusCode::GATEWAY_TIMEOUT,
                json!({"error": "command_timeout", "message": "The browser did not respond before the command timed out."}),
                false,
            )
        }
    }
}

async fn agent_upload_reservation(
    State(state): State<Arc<BridgeState>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Some(response) = authorize_connected_agent(&state, &headers).await {
        return response;
    }
    let value = match read_json(&headers, body).await {
        Ok(value) => value,
        Err(message) => return invalid_request(&state, message, false),
    };
    let Some(path) = value.get("path").and_then(Value::as_str) else {
        return invalid_request(&state, "a local upload path is required", false);
    };
    match state.transfers.reserve_upload(PathBuf::from(path)).await {
        Ok(result) => json_response(&state, StatusCode::OK, json!(result), false),
        Err(_) => invalid_request(&state, "upload path is unavailable", false),
    }
}

async fn agent_output_reservation(
    State(state): State<Arc<BridgeState>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Some(response) = authorize_connected_agent(&state, &headers).await {
        return response;
    }
    let value = match read_json(&headers, body).await {
        Ok(value) => value,
        Err(message) => return invalid_request(&state, message, false),
    };
    let Some(path) = value.get("path").and_then(Value::as_str) else {
        return invalid_request(&state, "a local output path is required", false);
    };
    let overwrite = value
        .get("overwrite")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    match state
        .transfers
        .reserve_output(PathBuf::from(path), overwrite)
        .await
    {
        Ok(result) => json_response(&state, StatusCode::OK, json!(result), false),
        Err(_) => invalid_request(&state, "output path is unavailable", false),
    }
}

async fn agent_disconnect(State(state): State<Arc<BridgeState>>, headers: HeaderMap) -> Response {
    if !has_agent_token(&state, &headers) {
        return json_response(
            &state,
            StatusCode::UNAUTHORIZED,
            json!({"error": "invalid_agent_session"}),
            false,
        );
    }
    state
        .shutdown(BridgeStatus::Rejected, "Agent disconnected.")
        .await;
    json_response(&state, StatusCode::OK, json!({"status": "rejected"}), false)
}

async fn authorize_connected_agent(
    state: &Arc<BridgeState>,
    headers: &HeaderMap,
) -> Option<Response> {
    if !has_agent_token(state, headers) {
        return Some(json_response(
            state,
            StatusCode::UNAUTHORIZED,
            json!({"error": "invalid_agent_session"}),
            false,
        ));
    }
    if state.inner.lock().await.status != BridgeStatus::Connected {
        return Some(json_response(
            state,
            StatusCode::CONFLICT,
            json!({"error": "browser_disconnected"}),
            false,
        ));
    }
    None
}

async fn authorize_browser_pairing(
    state: &Arc<BridgeState>,
    headers: &HeaderMap,
) -> Option<Response> {
    if !has_browser_origin(state, headers) {
        return Some(json_response(
            state,
            StatusCode::FORBIDDEN,
            json!({"error": "origin_not_allowed"}),
            false,
        ));
    }
    let inner = state.inner.lock().await;
    if !has_token(headers, inner.pairing_token.as_ref()) {
        return Some(json_response(
            state,
            StatusCode::UNAUTHORIZED,
            json!({"error": "invalid_pairing_token"}),
            true,
        ));
    }
    None
}

async fn authorize_browser_session(
    state: &Arc<BridgeState>,
    headers: &HeaderMap,
) -> Option<Response> {
    if !has_browser_origin(state, headers) {
        return Some(json_response(
            state,
            StatusCode::FORBIDDEN,
            json!({"error": "origin_not_allowed"}),
            false,
        ));
    }
    let inner = state.inner.lock().await;
    if inner.status != BridgeStatus::Connected || !has_token(headers, inner.browser_token.as_ref())
    {
        return Some(json_response(
            state,
            StatusCode::UNAUTHORIZED,
            json!({"error": "invalid_browser_session"}),
            true,
        ));
    }
    None
}

fn has_browser_origin(state: &BridgeState, headers: &HeaderMap) -> bool {
    headers.get(ORIGIN).and_then(|value| value.to_str().ok())
        == Some(state.expected_origin.as_str())
}

fn has_agent_token(state: &BridgeState, headers: &HeaderMap) -> bool {
    has_token(headers, Some(&state.agent_token))
}

fn has_token(headers: &HeaderMap, expected: Option<&Secret>) -> bool {
    let received = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);
    secrets_equal(received, expected.map(Secret::expose))
}

async fn read_json(headers: &HeaderMap, body: Body) -> Result<Value, &'static str> {
    let valid_content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("application/json"));
    if !valid_content_type {
        return Err("expected application/json");
    }
    let bytes = to_bytes(body, MAX_JSON_BYTES)
        .await
        .map_err(|_| "JSON request is too large")?;
    serde_json::from_slice(&bytes).map_err(|_| "JSON request is malformed")
}

fn cache_result(inner: &mut BridgeInner, request_id: String, value: Value) {
    if !inner.result_cache.contains_key(&request_id) {
        inner.result_order.push_back(request_id.clone());
    }
    inner.result_cache.insert(request_id, value);
    while inner.result_order.len() > MAX_RESULT_CACHE {
        if let Some(oldest) = inner.result_order.pop_front() {
            inner.result_cache.remove(&oldest);
        }
    }
}

fn json_response(state: &BridgeState, status: StatusCode, value: Value, browser: bool) -> Response {
    let serialized = serde_json::to_vec(&value).expect("JSON values serialize");
    let length = serialized.len();
    let mut response = Response::new(Body::from(serialized));
    *response.status_mut() = status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    response.headers_mut().insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&length.to_string()).expect("numeric header"),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if browser {
        add_browser_headers(state, response.headers_mut());
    }
    response
}

fn invalid_request(state: &BridgeState, message: &str, browser: bool) -> Response {
    json_response(
        state,
        StatusCode::BAD_REQUEST,
        json!({"error": "invalid_request", "message": message}),
        browser,
    )
}

fn empty_browser_response(state: &BridgeState, status: StatusCode) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    add_browser_headers(state, response.headers_mut());
    response
}

fn stream_response(state: &BridgeState, body: Body) -> Response {
    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::OK;
    add_browser_headers(state, response.headers_mut());
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/x-ndjson; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn add_browser_headers(state: &BridgeState, headers: &mut HeaderMap) {
    headers.insert(
        "access-control-allow-origin",
        HeaderValue::from_str(&state.expected_origin).expect("validated origin header"),
    );
    headers.insert(
        "access-control-allow-methods",
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    headers.insert(
        "access-control-allow-headers",
        HeaderValue::from_static("Authorization, Content-Type"),
    );
    headers.insert(
        "access-control-expose-headers",
        HeaderValue::from_static("Content-Length, Content-Type, X-GifGun-File-Name"),
    );
    headers.insert(
        "access-control-allow-private-network",
        HeaderValue::from_static("true"),
    );
    headers.insert("access-control-max-age", HeaderValue::from_static("600"));
    headers.insert(VARY, HeaderValue::from_static("Origin"));
}

fn spawn_heartbeat(state: Arc<BridgeState>) {
    tokio::spawn(async move {
        let mut shutdown = state.shutdown.subscribe();
        let mut interval = tokio::time::interval(HEARTBEAT_INTERVAL);
        interval.tick().await;
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let _ = state.send_stream_record(json!({"type": "heartbeat", "at": now_millis()})).await;
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
    });
}

fn spawn_expiry(state: Arc<BridgeState>) {
    tokio::spawn(async move {
        if state.pairing_expires_at != u64::MAX {
            let wait = Duration::from_millis(state.pairing_expires_at.saturating_sub(now_millis()));
            tokio::time::sleep(wait).await;
            let should_expire = matches!(
                state.inner.lock().await.status,
                BridgeStatus::Waiting | BridgeStatus::ApprovalPending | BridgeStatus::Incompatible
            );
            if should_expire {
                state
                    .shutdown(BridgeStatus::Expired, "Pairing instruction expired.")
                    .await;
                return;
            }
        }
        if state.session_expires_at != u64::MAX {
            let wait = Duration::from_millis(state.session_expires_at.saturating_sub(now_millis()));
            tokio::time::sleep(wait).await;
            state
                .shutdown(BridgeStatus::Expired, "Agent session expired.")
                .await;
        }
    });
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
