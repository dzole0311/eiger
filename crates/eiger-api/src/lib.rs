use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{
        Path, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use eiger_config::AppConfig;
use eiger_metrics::render_prometheus;
use eiger_pool::{PoolError, SessionHandle, SessionOverrides, SessionPool};
use eiger_stealth::baseline_scripts;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::time::{Duration, sleep};
use tokio_tungstenite::{connect_async, tungstenite::Message as UpstreamMessage};
use tracing::{debug, warn};
use uuid::Uuid;

const EIGER_CDP_ID_START: u64 = 9_000_000_000;

type UpstreamSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type UpstreamSink = futures_util::stream::SplitSink<UpstreamSocket, UpstreamMessage>;

#[derive(Clone)]
pub struct ApiState {
    pool: Arc<SessionPool>,
    token: Option<String>,
}

impl ApiState {
    pub fn new(pool: Arc<SessionPool>, config: &AppConfig) -> Self {
        Self {
            pool,
            token: config.auth.token.clone(),
        }
    }
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/", get(connect_new_session))
        .route("/session", get(connect_new_session))
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/sessions/{id}", get(get_session).delete(delete_session))
        .route("/sessions/{id}/cdp", get(connect_existing_session))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionQuery {
    token: Option<String>,
    stealth: Option<bool>,
    launch: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionRequest {
    stealth_enabled: Option<bool>,
    extra_chrome_args: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LaunchQuery {
    args: Option<Vec<String>>,
    stealth: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthResponse {
    status: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorResponse {
    error: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreatedSessionResponse {
    id: Uuid,
    pid: u32,
    cdp_ws_url: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn create_session(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<SessionQuery>,
    payload: Option<Json<CreateSessionRequest>>,
) -> Response {
    if !authorized(&state, &headers, query.token.as_deref()) {
        return api_error(StatusCode::UNAUTHORIZED, "unauthorized");
    }

    let payload = payload.map(|Json(payload)| payload);
    let overrides = match session_overrides(&query, payload) {
        Ok(overrides) => overrides,
        Err(error) => return api_error(StatusCode::BAD_REQUEST, error),
    };

    match state.pool.create_session(overrides).await {
        Ok(handle) => {
            let id = handle.id();
            let token = request_token(&headers, query.token.as_deref());
            let cdp_ws_url = cdp_ws_url(&headers, id, token.as_deref());
            let created_at = state
                .pool
                .get_session(id)
                .await
                .map(|session| session.created_at)
                .unwrap_or_else(chrono::Utc::now);
            (
                StatusCode::CREATED,
                Json(CreatedSessionResponse {
                    id,
                    pid: handle.pid(),
                    cdp_ws_url,
                    created_at,
                }),
            )
                .into_response()
        }
        Err(error) => pool_error(error),
    }
}

async fn list_sessions(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<SessionQuery>,
) -> Response {
    if !authorized(&state, &headers, query.token.as_deref()) {
        return api_error(StatusCode::UNAUTHORIZED, "unauthorized");
    }

    Json(state.pool.list_sessions().await).into_response()
}

async fn get_session(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<SessionQuery>,
    Path(id): Path<Uuid>,
) -> Response {
    if !authorized(&state, &headers, query.token.as_deref()) {
        return api_error(StatusCode::UNAUTHORIZED, "unauthorized");
    }

    match state.pool.get_session(id).await {
        Some(session) => Json(session).into_response(),
        None => api_error(StatusCode::NOT_FOUND, "session not found"),
    }
}

async fn delete_session(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<SessionQuery>,
    Path(id): Path<Uuid>,
) -> Response {
    if !authorized(&state, &headers, query.token.as_deref()) {
        return api_error(StatusCode::UNAUTHORIZED, "unauthorized");
    }

    if state.pool.terminate_session(id, "delete requested").await {
        StatusCode::NO_CONTENT.into_response()
    } else {
        api_error(StatusCode::NOT_FOUND, "session not found")
    }
}

async fn metrics(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<SessionQuery>,
) -> Response {
    if !authorized(&state, &headers, query.token.as_deref()) {
        return api_error(StatusCode::UNAUTHORIZED, "unauthorized");
    }

    let metrics = state.pool.metrics().await;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/plain; version=0.0.4")
        .body(Body::from(render_prometheus(&metrics)))
        .unwrap()
}

async fn connect_new_session(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<SessionQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    if !authorized(&state, &headers, query.token.as_deref()) {
        return api_error(StatusCode::UNAUTHORIZED, "unauthorized");
    }

    let overrides = match session_overrides(&query, None) {
        Ok(overrides) => overrides,
        Err(error) => return api_error(StatusCode::BAD_REQUEST, error),
    };

    match state.pool.create_session(overrides).await {
        Ok(handle) => {
            let pool = state.pool.clone();
            ws.on_upgrade(move |socket| proxy_and_recycle(socket, pool, handle))
        }
        Err(error) => pool_error(error),
    }
}

async fn connect_existing_session(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<SessionQuery>,
    Path(id): Path<Uuid>,
    ws: WebSocketUpgrade,
) -> Response {
    if !authorized(&state, &headers, query.token.as_deref()) {
        return api_error(StatusCode::UNAUTHORIZED, "unauthorized");
    }

    match state.pool.connect_session(id).await {
        Ok(handle) => {
            let pool = state.pool.clone();
            ws.on_upgrade(move |socket| proxy_and_recycle(socket, pool, handle))
        }
        Err(error) => pool_error(error),
    }
}

async fn proxy_and_recycle(socket: WebSocket, pool: Arc<SessionPool>, handle: SessionHandle) {
    let id = handle.id();
    handle.mark_in_use().await;

    if let Err(error) =
        proxy_websocket(socket, handle.browser_ws_url(), handle.stealth_enabled()).await
    {
        warn!(%error, %id, "cdp websocket proxy ended with error");
    } else {
        debug!(%id, "cdp websocket proxy closed");
    }

    pool.terminate_session(id, "cdp websocket disconnected")
        .await;
}

async fn proxy_websocket(
    client_socket: WebSocket,
    upstream_url: &str,
    stealth_enabled: bool,
) -> Result<(), tokio_tungstenite::tungstenite::Error> {
    let (upstream_socket, _) = connect_async(upstream_url).await?;
    let (mut client_sender, mut client_receiver) = client_socket.split();
    let (upstream_sender, mut upstream_receiver) = upstream_socket.split();
    let upstream_sender = Arc::new(Mutex::new(upstream_sender));
    let eiger_cdp_id = Arc::new(AtomicU64::new(EIGER_CDP_ID_START));

    let client_to_upstream = async {
        while let Some(message) = client_receiver.next().await {
            let message = match message {
                Ok(message) => message,
                Err(_) => break,
            };

            if stealth_enabled && let Some(session_id) = client_message_stealth_session(&message) {
                let mut sender = upstream_sender.lock().await;
                send_stealth_commands(&mut sender, &eiger_cdp_id, &session_id).await?;
                sleep(Duration::from_millis(50)).await;
            }

            let Some(message) = axum_to_upstream_message(message) else {
                break;
            };

            upstream_sender.lock().await.send(message).await?;
        }
        let _ = upstream_sender.lock().await.close().await;
        Ok::<(), tokio_tungstenite::tungstenite::Error>(())
    };

    let upstream_to_client = async {
        while let Some(message) = upstream_receiver.next().await {
            let message = message?;
            let Some(message) =
                upstream_to_axum_message(message, stealth_enabled, &upstream_sender, &eiger_cdp_id)
                    .await?
            else {
                continue;
            };

            if client_sender.send(message).await.is_err() {
                break;
            }
        }
        let _ = client_sender.close().await;
        Ok::<(), tokio_tungstenite::tungstenite::Error>(())
    };

    tokio::select! {
        result = client_to_upstream => result,
        result = upstream_to_client => result,
    }
}

fn axum_to_upstream_message(message: Message) -> Option<UpstreamMessage> {
    match message {
        Message::Text(text) => Some(UpstreamMessage::Text(text.to_string().into())),
        Message::Binary(bytes) => Some(UpstreamMessage::Binary(bytes)),
        Message::Ping(bytes) => Some(UpstreamMessage::Ping(bytes)),
        Message::Pong(bytes) => Some(UpstreamMessage::Pong(bytes)),
        Message::Close(_) => Some(UpstreamMessage::Close(None)),
    }
}

async fn upstream_to_axum_message(
    message: UpstreamMessage,
    stealth_enabled: bool,
    upstream_sender: &Arc<Mutex<UpstreamSink>>,
    eiger_cdp_id: &Arc<AtomicU64>,
) -> Result<Option<Message>, tokio_tungstenite::tungstenite::Error> {
    match message {
        UpstreamMessage::Text(text) => {
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                if is_eiger_cdp_response(&value) {
                    if let Some(error) = value.get("error") {
                        warn!(%error, "proxied stealth injection command failed");
                    }
                    return Ok(None);
                }

                if stealth_enabled {
                    inject_stealth_from_proxy_event(&value, upstream_sender, eiger_cdp_id).await?;
                }
            }
            Ok(Some(Message::Text(text.to_string().into())))
        }
        UpstreamMessage::Binary(bytes) => Ok(Some(Message::Binary(bytes))),
        UpstreamMessage::Ping(bytes) => Ok(Some(Message::Ping(bytes))),
        UpstreamMessage::Pong(bytes) => Ok(Some(Message::Pong(bytes))),
        UpstreamMessage::Close(_) => Ok(Some(Message::Close(None))),
        UpstreamMessage::Frame(_) => Ok(None),
    }
}

async fn inject_stealth_from_proxy_event(
    value: &Value,
    upstream_sender: &Arc<Mutex<UpstreamSink>>,
    eiger_cdp_id: &Arc<AtomicU64>,
) -> Result<(), tokio_tungstenite::tungstenite::Error> {
    if value.get("method").and_then(Value::as_str) != Some("Target.attachedToTarget") {
        return Ok(());
    }

    let Some(params) = value.get("params") else {
        return Ok(());
    };
    let Some(session_id) = params.get("sessionId").and_then(Value::as_str) else {
        return Ok(());
    };
    let target_type = params
        .get("targetInfo")
        .and_then(|target| target.get("type"))
        .and_then(Value::as_str);

    if !matches!(target_type, Some("page" | "iframe")) {
        return Ok(());
    }

    debug!(
        session_id,
        target_type, "injecting stealth script through proxied cdp session"
    );

    let mut sender = upstream_sender.lock().await;
    send_stealth_commands(&mut sender, eiger_cdp_id, session_id).await?;

    sleep(Duration::from_millis(200)).await;

    Ok(())
}

async fn send_stealth_commands(
    sender: &mut UpstreamSink,
    eiger_cdp_id: &Arc<AtomicU64>,
    session_id: &str,
) -> Result<(), tokio_tungstenite::tungstenite::Error> {
    send_proxy_cdp_command(
        sender,
        eiger_cdp_id,
        Some(session_id),
        "Page.enable",
        json!({}),
    )
    .await?;

    for script in baseline_scripts() {
        send_proxy_cdp_command(
            sender,
            eiger_cdp_id,
            Some(session_id),
            "Page.addScriptToEvaluateOnNewDocument",
            json!({ "source": script.source, "runImmediately": true }),
        )
        .await?;
        send_proxy_cdp_command(
            sender,
            eiger_cdp_id,
            Some(session_id),
            "Runtime.evaluate",
            json!({ "expression": script.source }),
        )
        .await?;
    }

    Ok(())
}

fn client_message_stealth_session(message: &Message) -> Option<String> {
    let Message::Text(text) = message else {
        return None;
    };
    let value: Value = serde_json::from_str(text).ok()?;
    let method = value.get("method").and_then(Value::as_str)?;
    let has_session = value.get("sessionId").and_then(Value::as_str).is_some();
    if method.starts_with("Page.") || method.starts_with("Runtime.") {
        debug!(method, has_session, "proxied client cdp command");
    }

    if !matches!(
        method,
        "Page.navigate" | "Page.reload" | "Page.printToPDF" | "Page.captureScreenshot"
    ) {
        return None;
    }

    value
        .get("sessionId")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

async fn send_proxy_cdp_command(
    sender: &mut UpstreamSink,
    eiger_cdp_id: &Arc<AtomicU64>,
    session_id: Option<&str>,
    method: &'static str,
    params: Value,
) -> Result<(), tokio_tungstenite::tungstenite::Error> {
    let id = eiger_cdp_id.fetch_add(1, Ordering::Relaxed);
    let mut request = json!({
        "id": id,
        "method": method,
        "params": params,
    });

    if let Some(session_id) = session_id {
        request["sessionId"] = Value::String(session_id.to_owned());
    }

    sender
        .send(UpstreamMessage::Text(request.to_string().into()))
        .await
}

fn is_eiger_cdp_response(value: &Value) -> bool {
    value
        .get("id")
        .and_then(Value::as_u64)
        .is_some_and(|id| id >= EIGER_CDP_ID_START)
}

fn authorized(state: &ApiState, headers: &HeaderMap, query_token: Option<&str>) -> bool {
    let Some(expected) = state.token.as_deref() else {
        return true;
    };

    if query_token == Some(expected) {
        return true;
    }

    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        == Some(expected)
}

fn request_token(headers: &HeaderMap, query_token: Option<&str>) -> Option<String> {
    query_token.map(ToOwned::to_owned).or_else(|| {
        headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(ToOwned::to_owned)
    })
}

fn session_overrides(
    query: &SessionQuery,
    payload: Option<CreateSessionRequest>,
) -> Result<SessionOverrides, String> {
    let launch = parse_launch_query(query.launch.as_deref())?;

    let mut extra_chrome_args = launch
        .as_ref()
        .and_then(|launch| launch.args.clone())
        .unwrap_or_default();

    let payload_stealth = payload.as_ref().and_then(|payload| payload.stealth_enabled);
    let query_stealth = query
        .stealth
        .or_else(|| launch.as_ref().and_then(|launch| launch.stealth));

    if let Some(payload) = payload
        && let Some(args) = payload.extra_chrome_args
    {
        extra_chrome_args.extend(args);
    }

    Ok(SessionOverrides {
        stealth_enabled: payload_stealth.or(query_stealth),
        extra_chrome_args,
    })
}

fn parse_launch_query(raw_launch: Option<&str>) -> Result<Option<LaunchQuery>, String> {
    let Some(raw_launch) = raw_launch.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };

    let value: Value = serde_json::from_str(raw_launch)
        .map_err(|error| format!("invalid launch query JSON: {error}"))?;

    serde_json::from_value(value)
        .map(Some)
        .map_err(|error| format!("invalid launch query shape: {error}"))
}

fn cdp_ws_url(headers: &HeaderMap, id: Uuid, query_token: Option<&str>) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("127.0.0.1:3000");
    let forwarded_proto = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("http");
    let scheme = if forwarded_proto.eq_ignore_ascii_case("https") {
        "wss"
    } else {
        "ws"
    };

    let mut url = format!("{scheme}://{host}/sessions/{id}/cdp");
    if let Some(token) = query_token {
        url.push_str("?token=");
        url.push_str(&urlencoding::encode(token));
    }
    url
}

fn pool_error(error: PoolError) -> Response {
    match error {
        PoolError::AtCapacity | PoolError::CapacityTimeout => {
            api_error(StatusCode::SERVICE_UNAVAILABLE, error.to_string())
        }
        PoolError::NotFound(_) => api_error(StatusCode::NOT_FOUND, error.to_string()),
        PoolError::NotConnectable(_) => api_error(StatusCode::CONFLICT, error.to_string()),
        PoolError::Browser(_) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
    }
}

fn api_error(status: StatusCode, error: impl Into<String>) -> Response {
    (
        status,
        Json(ErrorResponse {
            error: error.into(),
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_launch_json_query_overrides() {
        let query = SessionQuery {
            token: None,
            stealth: None,
            launch: Some(
                r#"{"args":["--proxy-server=http://proxy.local:8080"],"stealth":false}"#.to_owned(),
            ),
        };

        let overrides = session_overrides(&query, None).expect("valid launch query");

        assert_eq!(overrides.stealth_enabled, Some(false));
        assert_eq!(
            overrides.extra_chrome_args,
            vec!["--proxy-server=http://proxy.local:8080"]
        );
    }

    #[test]
    fn query_stealth_overrides_launch_stealth() {
        let query = SessionQuery {
            token: None,
            stealth: Some(true),
            launch: Some(r#"{"stealth":false}"#.to_owned()),
        };

        let overrides = session_overrides(&query, None).expect("valid launch query");

        assert_eq!(overrides.stealth_enabled, Some(true));
    }

    #[test]
    fn cdp_url_can_carry_bearer_token_from_prewarm_request() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "eiger.local:3000".parse().unwrap());
        headers.insert(header::AUTHORIZATION, "Bearer secret".parse().unwrap());
        let id = Uuid::nil();
        let token = request_token(&headers, None);

        assert_eq!(
            cdp_ws_url(&headers, id, token.as_deref()),
            "ws://eiger.local:3000/sessions/00000000-0000-0000-0000-000000000000/cdp?token=secret"
        );
    }
}
