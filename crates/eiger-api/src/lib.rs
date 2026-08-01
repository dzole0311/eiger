use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
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
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use eiger_config::AppConfig;
use eiger_metrics::render_prometheus;
use eiger_pool::{
    PoolError, PoolReadiness, SessionHandle, SessionInfo, SessionOverrides, SessionPool,
};
use eiger_stealth::baseline_scripts;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::time::{Duration, sleep, timeout};
use tokio_tungstenite::{connect_async, tungstenite::Message as UpstreamMessage};
use tracing::{debug, warn};
use utoipa::{OpenApi, ToSchema};
use utoipa_swagger_ui::SwaggerUi;
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
    liveness_router(state.clone()).merge(api_router(state))
}

pub fn liveness_router(state: ApiState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .with_state(state)
}

pub fn api_router(state: ApiState) -> Router {
    Router::new()
        .route("/", get(connect_new_session))
        .route("/session", get(connect_new_session))
        .route("/metrics", get(metrics))
        .route("/scrape", post(scrape))
        .route("/screenshot", post(screenshot))
        .route("/pdf", post(pdf))
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/sessions/view", get(view_sessions))
        .route("/sessions/{id}", get(get_session).delete(delete_session))
        .route("/sessions/{id}/cdp", get(connect_existing_session))
        .merge(SwaggerUi::new("/docs").url("/openapi.json", ApiDoc::openapi()))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionQuery {
    token: Option<String>,
    stealth: Option<bool>,
    launch: Option<String>,
    proxy: Option<String>,
    extension_paths: Option<String>,
    persistent_profile_id: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct CreateSessionRequest {
    stealth_enabled: Option<bool>,
    extra_chrome_args: Option<Vec<String>>,
    proxy: Option<String>,
    #[schema(value_type = Option<Vec<String>>)]
    extension_paths: Option<Vec<PathBuf>>,
    persistent_profile_id: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct ScrapeRequest {
    url: String,
    wait_until: Option<String>,
    timeout_ms: Option<u64>,
    proxy: Option<String>,
    #[schema(value_type = Option<Vec<String>>)]
    extension_paths: Option<Vec<PathBuf>>,
    persistent_profile_id: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct ScreenshotRequest {
    url: String,
    wait_until: Option<String>,
    timeout_ms: Option<u64>,
    full_page: Option<bool>,
    format: Option<String>,
    proxy: Option<String>,
    #[schema(value_type = Option<Vec<String>>)]
    extension_paths: Option<Vec<PathBuf>>,
    persistent_profile_id: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct PdfRequest {
    url: String,
    wait_until: Option<String>,
    timeout_ms: Option<u64>,
    format: Option<String>,
    landscape: Option<bool>,
    print_background: Option<bool>,
    proxy: Option<String>,
    #[schema(value_type = Option<Vec<String>>)]
    extension_paths: Option<Vec<PathBuf>>,
    persistent_profile_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LaunchQuery {
    args: Option<Vec<String>>,
    stealth: Option<bool>,
    proxy: Option<String>,
    extension_paths: Option<Vec<PathBuf>>,
    persistent_profile_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CdpTargetDescriptor {
    #[serde(rename = "type")]
    target_type: String,
    devtools_frontend_url: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthResponse {
    status: &'static str,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct ErrorResponse {
    error: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct CreatedSessionResponse {
    id: Uuid,
    pid: u32,
    cdp_ws_url: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct ScrapeResponse {
    html: String,
    title: String,
    url: String,
}

#[derive(Debug, Clone)]
struct PageLoadOptions {
    url: String,
    wait_until: WaitUntil,
    timeout: Duration,
}

#[derive(Debug, Clone, Copy)]
enum WaitUntil {
    Load,
    DomContentLoaded,
    NetworkIdle,
    NetworkAlmostIdle,
}

#[derive(Debug, Clone, Copy)]
enum ScreenshotFormat {
    Png,
    Jpeg,
}

#[derive(Debug)]
enum RestEndpointError {
    BadRequest(String),
    Pool(PoolError),
    CdpWebSocket(tokio_tungstenite::tungstenite::Error),
    CdpProtocol(String),
    CdpTimeout(&'static str),
    InvalidCdpResponse(String),
    Decode(String),
}

struct CdpConnection {
    socket: UpstreamSocket,
    next_id: u64,
    lifecycle_events: HashSet<String>,
}

#[derive(OpenApi)]
#[openapi(
    paths(
        scrape,
        screenshot,
        pdf,
        create_session,
        list_sessions,
        get_session,
        delete_session
    ),
    components(schemas(
        CreateSessionRequest,
        ScrapeRequest,
        ScreenshotRequest,
        PdfRequest,
        ScrapeResponse,
        CreatedSessionResponse,
        ErrorResponse,
        SessionInfo
    )),
    tags(
        (name = "rest", description = "REST convenience endpoints"),
        (name = "sessions", description = "Browser session management")
    )
)]
struct ApiDoc;

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn ready(State(state): State<ApiState>) -> Response {
    let readiness = state.pool.readiness().await;
    let status = if readiness.can_accept_sessions {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (status, Json::<PoolReadiness>(readiness)).into_response()
}

#[utoipa::path(
    post,
    path = "/sessions",
    tag = "sessions",
    request_body = CreateSessionRequest,
    responses(
        (status = 201, description = "Session created", body = CreatedSessionResponse),
        (status = 400, description = "Invalid session overrides", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 503, description = "Pool at capacity", body = ErrorResponse)
    )
)]
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

#[utoipa::path(
    get,
    path = "/sessions",
    tag = "sessions",
    responses(
        (status = 200, description = "Active sessions", body = Vec<SessionInfo>),
        (status = 401, description = "Unauthorized", body = ErrorResponse)
    )
)]
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

async fn view_sessions(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<SessionQuery>,
) -> Response {
    if !authorized(&state, &headers, query.token.as_deref()) {
        return api_error(StatusCode::UNAUTHORIZED, "unauthorized");
    }

    let sessions = state.pool.list_sessions().await;
    let mut html = String::from(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>Eiger Sessions</title><style>body{font-family:system-ui,sans-serif;margin:24px;color:#111827}table{border-collapse:collapse;width:100%}th,td{border-bottom:1px solid #e5e7eb;padding:8px;text-align:left}th{font-size:12px;text-transform:uppercase;color:#6b7280}td{font-variant-numeric:tabular-nums}a{color:#0369a1}</style></head><body><h1>Sessions</h1><table><thead><tr><th>id</th><th>state</th><th>age</th><th>idle</th><th>rss</th><th>cpu</th><th>pid</th><th>devtools</th></tr></thead><tbody>"#,
    );

    for session in sessions {
        let devtools_url = session_devtools_link(&session)
            .await
            .unwrap_or_else(|| "#".to_owned());
        html.push_str("<tr>");
        html.push_str(&format!(
            "<td>{}</td>",
            escape_html(&session.id.to_string())
        ));
        html.push_str(&format!(
            "<td>{}</td>",
            escape_html(&session.state.to_string())
        ));
        html.push_str(&format!("<td>{}</td>", session.age_seconds));
        html.push_str(&format!("<td>{}</td>", session.idle_seconds));
        html.push_str(&format!(
            "<td>{}</td>",
            session
                .rss_bytes
                .map(|value| value.to_string())
                .unwrap_or_default()
        ));
        html.push_str(&format!(
            "<td>{}</td>",
            session
                .cpu_percent
                .map(|value| format!("{value:.1}"))
                .unwrap_or_default()
        ));
        html.push_str(&format!(
            "<td>{}</td>",
            session
                .pid
                .map(|value| value.to_string())
                .unwrap_or_default()
        ));
        html.push_str(&format!(
            r#"<td><a href="{}">devtools</a></td>"#,
            escape_html(&devtools_url)
        ));
        html.push_str("</tr>");
    }

    html.push_str("</tbody></table></body></html>");

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(html))
        .unwrap()
}

#[utoipa::path(
    get,
    path = "/sessions/{id}",
    tag = "sessions",
    params(("id" = Uuid, Path, description = "Session id")),
    responses(
        (status = 200, description = "Session", body = SessionInfo),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Session not found", body = ErrorResponse)
    )
)]
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

#[utoipa::path(
    delete,
    path = "/sessions/{id}",
    tag = "sessions",
    params(("id" = Uuid, Path, description = "Session id")),
    responses(
        (status = 204, description = "Session deleted"),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Session not found", body = ErrorResponse)
    )
)]
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

#[utoipa::path(
    post,
    path = "/scrape",
    tag = "rest",
    request_body = ScrapeRequest,
    responses(
        (status = 200, description = "Serialized page DOM", body = ScrapeResponse),
        (status = 400, description = "Invalid request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 502, description = "CDP failure", body = ErrorResponse),
        (status = 504, description = "CDP timeout", body = ErrorResponse)
    )
)]
async fn scrape(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<SessionQuery>,
    Json(payload): Json<ScrapeRequest>,
) -> Response {
    if !authorized(&state, &headers, query.token.as_deref()) {
        return api_error(StatusCode::UNAUTHORIZED, "unauthorized");
    }

    let proxy = payload.proxy.clone();
    let extension_paths = payload.extension_paths.clone();
    let persistent_profile_id = payload.persistent_profile_id.clone();
    let options = match page_load_options(payload.url, payload.wait_until, payload.timeout_ms) {
        Ok(options) => options,
        Err(error) => return rest_endpoint_error(error),
    };
    let overrides =
        match rest_session_overrides(&query, proxy, extension_paths, persistent_profile_id) {
            Ok(overrides) => overrides,
            Err(error) => return api_error(StatusCode::BAD_REQUEST, error),
        };

    match with_rest_session(&state, overrides, |handle| {
        scrape_with_session(handle, options)
    })
    .await
    {
        Ok(response) => Json(response).into_response(),
        Err(error) => rest_endpoint_error(error),
    }
}

#[utoipa::path(
    post,
    path = "/screenshot",
    tag = "rest",
    request_body = ScreenshotRequest,
    responses(
        (status = 200, description = "Screenshot bytes", content_type = "image/png"),
        (status = 400, description = "Invalid request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 502, description = "CDP failure", body = ErrorResponse),
        (status = 504, description = "CDP timeout", body = ErrorResponse)
    )
)]
async fn screenshot(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<SessionQuery>,
    Json(payload): Json<ScreenshotRequest>,
) -> Response {
    if !authorized(&state, &headers, query.token.as_deref()) {
        return api_error(StatusCode::UNAUTHORIZED, "unauthorized");
    }

    let proxy = payload.proxy.clone();
    let extension_paths = payload.extension_paths.clone();
    let persistent_profile_id = payload.persistent_profile_id.clone();
    let options = match page_load_options(payload.url, payload.wait_until, payload.timeout_ms) {
        Ok(options) => options,
        Err(error) => return rest_endpoint_error(error),
    };
    let format = match ScreenshotFormat::parse(payload.format.as_deref()) {
        Ok(format) => format,
        Err(error) => return rest_endpoint_error(error),
    };
    let full_page = payload.full_page.unwrap_or(false);
    let overrides =
        match rest_session_overrides(&query, proxy, extension_paths, persistent_profile_id) {
            Ok(overrides) => overrides,
            Err(error) => return api_error(StatusCode::BAD_REQUEST, error),
        };

    match with_rest_session(&state, overrides, |handle| {
        screenshot_with_session(handle, options, format, full_page)
    })
    .await
    {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, format.content_type())
            .body(Body::from(bytes))
            .unwrap(),
        Err(error) => rest_endpoint_error(error),
    }
}

#[utoipa::path(
    post,
    path = "/pdf",
    tag = "rest",
    request_body = PdfRequest,
    responses(
        (status = 200, description = "PDF bytes", content_type = "application/pdf"),
        (status = 400, description = "Invalid request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 502, description = "CDP failure", body = ErrorResponse),
        (status = 504, description = "CDP timeout", body = ErrorResponse)
    )
)]
async fn pdf(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<SessionQuery>,
    Json(payload): Json<PdfRequest>,
) -> Response {
    if !authorized(&state, &headers, query.token.as_deref()) {
        return api_error(StatusCode::UNAUTHORIZED, "unauthorized");
    }

    let proxy = payload.proxy.clone();
    let extension_paths = payload.extension_paths.clone();
    let persistent_profile_id = payload.persistent_profile_id.clone();
    let options = match page_load_options(
        payload.url.clone(),
        payload.wait_until.clone(),
        payload.timeout_ms,
    ) {
        Ok(options) => options,
        Err(error) => return rest_endpoint_error(error),
    };
    let print_options = match pdf_print_options(&payload) {
        Ok(options) => options,
        Err(error) => return rest_endpoint_error(error),
    };
    let overrides =
        match rest_session_overrides(&query, proxy, extension_paths, persistent_profile_id) {
            Ok(overrides) => overrides,
            Err(error) => return api_error(StatusCode::BAD_REQUEST, error),
        };

    match with_rest_session(&state, overrides, |handle| {
        pdf_with_session(handle, options, print_options)
    })
    .await
    {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/pdf")
            .body(Body::from(bytes))
            .unwrap(),
        Err(error) => rest_endpoint_error(error),
    }
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

async fn with_rest_session<T, F, Fut>(
    state: &ApiState,
    overrides: SessionOverrides,
    operation: F,
) -> Result<T, RestEndpointError>
where
    F: FnOnce(SessionHandle) -> Fut,
    Fut: std::future::Future<Output = Result<T, RestEndpointError>>,
{
    let handle = state
        .pool
        .create_session(overrides)
        .await
        .map_err(RestEndpointError::Pool)?;
    let id = handle.id();
    handle.mark_in_use().await;

    let result = operation(handle).await;
    state
        .pool
        .terminate_session(id, "rest endpoint completed")
        .await;
    result
}

async fn scrape_with_session(
    handle: SessionHandle,
    options: PageLoadOptions,
) -> Result<ScrapeResponse, RestEndpointError> {
    let mut cdp = CdpConnection::connect(handle.browser_ws_url()).await?;
    let session_id = cdp.prepare_page_target(options.timeout).await?;
    navigate_page(&mut cdp, &session_id, &options).await?;

    let result = cdp
        .command(
            Some(&session_id),
            "Runtime.evaluate",
            json!({
                "expression": r#"(() => ({ html: document.documentElement ? document.documentElement.outerHTML : "", title: document.title, url: location.href }))()"#,
                "returnByValue": true
            }),
            options.timeout,
        )
        .await?;
    let value = result
        .get("result")
        .and_then(|result| result.get("value"))
        .ok_or_else(|| {
            RestEndpointError::InvalidCdpResponse(
                "Runtime.evaluate missing result.value".to_owned(),
            )
        })?;

    Ok(ScrapeResponse {
        html: value
            .get("html")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        title: value
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        url: value
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    })
}

async fn screenshot_with_session(
    handle: SessionHandle,
    options: PageLoadOptions,
    format: ScreenshotFormat,
    full_page: bool,
) -> Result<Vec<u8>, RestEndpointError> {
    let mut cdp = CdpConnection::connect(handle.browser_ws_url()).await?;
    let session_id = cdp.prepare_page_target(options.timeout).await?;
    navigate_page(&mut cdp, &session_id, &options).await?;

    if full_page {
        set_full_page_viewport(&mut cdp, &session_id, options.timeout).await?;
    }

    let result = cdp
        .command(
            Some(&session_id),
            "Page.captureScreenshot",
            json!({
                "format": format.cdp_name(),
                "fromSurface": true,
                "captureBeyondViewport": full_page
            }),
            options.timeout,
        )
        .await?;

    decode_cdp_data(&result)
}

async fn pdf_with_session(
    handle: SessionHandle,
    options: PageLoadOptions,
    print_options: Value,
) -> Result<Vec<u8>, RestEndpointError> {
    let mut cdp = CdpConnection::connect(handle.browser_ws_url()).await?;
    let session_id = cdp.prepare_page_target(options.timeout).await?;
    navigate_page(&mut cdp, &session_id, &options).await?;

    let result = cdp
        .command(
            Some(&session_id),
            "Page.printToPDF",
            print_options,
            options.timeout,
        )
        .await?;

    decode_cdp_data(&result)
}

async fn navigate_page(
    cdp: &mut CdpConnection,
    session_id: &str,
    options: &PageLoadOptions,
) -> Result<(), RestEndpointError> {
    cdp.lifecycle_events.clear();
    let result = cdp
        .command(
            Some(session_id),
            "Page.navigate",
            json!({ "url": options.url }),
            options.timeout,
        )
        .await?;

    if let Some(error) = result.get("errorText").and_then(Value::as_str) {
        return Err(RestEndpointError::CdpProtocol(format!(
            "Page.navigate failed: {error}"
        )));
    }

    cdp.wait_for_lifecycle(session_id, options.wait_until, options.timeout)
        .await
}

async fn set_full_page_viewport(
    cdp: &mut CdpConnection,
    session_id: &str,
    wait: Duration,
) -> Result<(), RestEndpointError> {
    let metrics = cdp
        .command(Some(session_id), "Page.getLayoutMetrics", json!({}), wait)
        .await?;
    let size = metrics
        .get("cssContentSize")
        .or_else(|| metrics.get("contentSize"))
        .ok_or_else(|| {
            RestEndpointError::InvalidCdpResponse(
                "Page.getLayoutMetrics missing content size".to_owned(),
            )
        })?;
    let width = positive_dimension(size.get("width"), "width")?;
    let height = positive_dimension(size.get("height"), "height")?;

    cdp.command(
        Some(session_id),
        "Emulation.setDeviceMetricsOverride",
        json!({
            "mobile": false,
            "width": width,
            "height": height,
            "deviceScaleFactor": 1,
            "screenWidth": width,
            "screenHeight": height
        }),
        wait,
    )
    .await?;
    Ok(())
}

fn page_load_options(
    url: String,
    wait_until: Option<String>,
    timeout_ms: Option<u64>,
) -> Result<PageLoadOptions, RestEndpointError> {
    let url = url.trim().to_owned();
    if url.is_empty() {
        return Err(RestEndpointError::BadRequest("url is required".to_owned()));
    }

    let timeout = Duration::from_millis(timeout_ms.unwrap_or(30_000));
    if timeout.is_zero() {
        return Err(RestEndpointError::BadRequest(
            "timeoutMs must be greater than 0".to_owned(),
        ));
    }

    Ok(PageLoadOptions {
        url,
        wait_until: WaitUntil::parse(wait_until.as_deref())?,
        timeout,
    })
}

fn pdf_print_options(request: &PdfRequest) -> Result<Value, RestEndpointError> {
    let mut params = json!({
        "landscape": request.landscape.unwrap_or(false),
        "printBackground": request.print_background.unwrap_or(false),
        "transferMode": "ReturnAsBase64"
    });

    if let Some(format) = request.format.as_deref() {
        let (width, height) = pdf_paper_size(format)?;
        params["paperWidth"] = json!(width);
        params["paperHeight"] = json!(height);
    }

    Ok(params)
}

fn pdf_paper_size(format: &str) -> Result<(f64, f64), RestEndpointError> {
    let normalized = format.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "letter" => Ok((8.5, 11.0)),
        "legal" => Ok((8.5, 14.0)),
        "tabloid" | "ledger" => Ok((11.0, 17.0)),
        "a0" => Ok((33.1, 46.8)),
        "a1" => Ok((23.4, 33.1)),
        "a2" => Ok((16.54, 23.4)),
        "a3" => Ok((11.7, 16.54)),
        "a4" => Ok((8.27, 11.7)),
        "a5" => Ok((5.83, 8.27)),
        "a6" => Ok((4.13, 5.83)),
        _ => Err(RestEndpointError::BadRequest(format!(
            "unsupported pdf format: {format}"
        ))),
    }
}

fn positive_dimension(value: Option<&Value>, name: &str) -> Result<u64, RestEndpointError> {
    let value = value.and_then(Value::as_f64).ok_or_else(|| {
        RestEndpointError::InvalidCdpResponse(format!(
            "Page.getLayoutMetrics missing numeric {name}"
        ))
    })?;
    Ok(value.ceil().max(1.0) as u64)
}

fn decode_cdp_data(result: &Value) -> Result<Vec<u8>, RestEndpointError> {
    let data = result.get("data").and_then(Value::as_str).ok_or_else(|| {
        RestEndpointError::InvalidCdpResponse("CDP result missing data".to_owned())
    })?;

    BASE64
        .decode(data)
        .map_err(|error| RestEndpointError::Decode(error.to_string()))
}

impl CdpConnection {
    async fn connect(ws_url: &str) -> Result<Self, RestEndpointError> {
        let (socket, _) = connect_async(ws_url)
            .await
            .map_err(RestEndpointError::CdpWebSocket)?;

        Ok(Self {
            socket,
            next_id: 1,
            lifecycle_events: HashSet::new(),
        })
    }

    async fn prepare_page_target(&mut self, wait: Duration) -> Result<String, RestEndpointError> {
        let created = self
            .command(
                None,
                "Target.createTarget",
                json!({ "url": "about:blank" }),
                wait,
            )
            .await?;
        let target_id = required_string(&created, "targetId")?;
        let attached = self
            .command(
                None,
                "Target.attachToTarget",
                json!({ "targetId": target_id, "flatten": true }),
                wait,
            )
            .await?;
        let session_id = required_string(&attached, "sessionId")?;

        self.command(Some(&session_id), "Page.enable", json!({}), wait)
            .await?;
        self.command(
            Some(&session_id),
            "Page.setLifecycleEventsEnabled",
            json!({ "enabled": true }),
            wait,
        )
        .await?;

        Ok(session_id)
    }

    async fn command(
        &mut self,
        session_id: Option<&str>,
        method: &'static str,
        params: Value,
        wait: Duration,
    ) -> Result<Value, RestEndpointError> {
        let id = self.next_id;
        self.next_id += 1;
        let mut request = json!({
            "id": id,
            "method": method,
            "params": params,
        });

        if let Some(session_id) = session_id {
            request["sessionId"] = Value::String(session_id.to_owned());
        }

        self.socket
            .send(UpstreamMessage::Text(request.to_string().into()))
            .await
            .map_err(RestEndpointError::CdpWebSocket)?;

        timeout(wait, async {
            loop {
                let value = self.next_json_message().await?;
                if value.get("id").and_then(Value::as_u64) != Some(id) {
                    continue;
                }

                if let Some(error) = value.get("error") {
                    return Err(RestEndpointError::CdpProtocol(error.to_string()));
                }

                return Ok(value.get("result").cloned().unwrap_or(Value::Null));
            }
        })
        .await
        .map_err(|_| RestEndpointError::CdpTimeout(method))?
    }

    async fn wait_for_lifecycle(
        &mut self,
        session_id: &str,
        wait_until: WaitUntil,
        wait: Duration,
    ) -> Result<(), RestEndpointError> {
        let expected = wait_until.lifecycle_name();
        if self.lifecycle_events.contains(expected) {
            return Ok(());
        }

        timeout(wait, async {
            loop {
                let value = self.next_json_message().await?;
                if value.get("sessionId").and_then(Value::as_str) != Some(session_id) {
                    continue;
                }
                if self.lifecycle_events.contains(expected) {
                    return Ok(());
                }
            }
        })
        .await
        .map_err(|_| RestEndpointError::CdpTimeout("Page.lifecycleEvent"))?
    }

    async fn next_json_message(&mut self) -> Result<Value, RestEndpointError> {
        loop {
            let message = self
                .socket
                .next()
                .await
                .ok_or_else(|| RestEndpointError::CdpProtocol("CDP websocket closed".to_owned()))?
                .map_err(RestEndpointError::CdpWebSocket)?;

            let UpstreamMessage::Text(text) = message else {
                continue;
            };
            let value: Value = serde_json::from_str(&text)
                .map_err(|error| RestEndpointError::CdpProtocol(error.to_string()))?;
            self.record_lifecycle_event(&value);
            return Ok(value);
        }
    }

    fn record_lifecycle_event(&mut self, value: &Value) {
        if value.get("method").and_then(Value::as_str) != Some("Page.lifecycleEvent") {
            return;
        }

        if let Some(name) = value
            .get("params")
            .and_then(|params| params.get("name"))
            .and_then(Value::as_str)
        {
            self.lifecycle_events.insert(name.to_owned());
        }
    }
}

impl WaitUntil {
    fn parse(value: Option<&str>) -> Result<Self, RestEndpointError> {
        let Some(value) = value else {
            return Ok(Self::Load);
        };
        match value.trim().to_ascii_lowercase().as_str() {
            "load" => Ok(Self::Load),
            "domcontentloaded" => Ok(Self::DomContentLoaded),
            "networkidle" | "networkidle0" => Ok(Self::NetworkIdle),
            "networkalmostidle" | "networkidle2" => Ok(Self::NetworkAlmostIdle),
            _ => Err(RestEndpointError::BadRequest(format!(
                "unsupported waitUntil value: {value}"
            ))),
        }
    }

    fn lifecycle_name(self) -> &'static str {
        match self {
            Self::Load => "load",
            Self::DomContentLoaded => "DOMContentLoaded",
            Self::NetworkIdle => "networkIdle",
            Self::NetworkAlmostIdle => "networkAlmostIdle",
        }
    }
}

impl ScreenshotFormat {
    fn parse(value: Option<&str>) -> Result<Self, RestEndpointError> {
        let Some(value) = value else {
            return Ok(Self::Png);
        };
        match value.trim().to_ascii_lowercase().as_str() {
            "png" => Ok(Self::Png),
            "jpeg" | "jpg" => Ok(Self::Jpeg),
            _ => Err(RestEndpointError::BadRequest(format!(
                "unsupported screenshot format: {value}"
            ))),
        }
    }

    fn cdp_name(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
        }
    }

    fn content_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
        }
    }
}

fn required_string(value: &Value, field: &str) -> Result<String, RestEndpointError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| RestEndpointError::InvalidCdpResponse(format!("CDP result missing {field}")))
}

async fn session_devtools_link(session: &SessionInfo) -> Option<String> {
    let browser_ws_url = session.browser_ws_url.as_deref()?;
    if let Some(link) = target_devtools_link(browser_ws_url).await {
        return Some(link);
    }

    devtools_url_from_ws(browser_ws_url)
}

async fn target_devtools_link(browser_ws_url: &str) -> Option<String> {
    let http_base = cdp_http_base_url(browser_ws_url)?;
    let targets: Vec<CdpTargetDescriptor> = reqwest::Client::new()
        .get(format!("{http_base}/json/list"))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;

    targets
        .into_iter()
        .find(|target| target.target_type == "page")
        .and_then(|target| target.devtools_frontend_url)
        .and_then(|url| {
            url.split_once("ws=")
                .map(|(_, ws)| format!("devtools://devtools/bundled/inspector.html?ws={ws}"))
        })
}

fn cdp_http_base_url(browser_ws_url: &str) -> Option<String> {
    let without_scheme = browser_ws_url
        .strip_prefix("ws://")
        .or_else(|| browser_ws_url.strip_prefix("wss://"))?;
    let (host, _) = without_scheme.split_once("/devtools/")?;
    Some(format!("http://{host}"))
}

fn devtools_url_from_ws(ws_url: &str) -> Option<String> {
    let ws = ws_url
        .strip_prefix("ws://")
        .or_else(|| ws_url.strip_prefix("wss://"))?;
    Some(format!(
        "devtools://devtools/bundled/inspector.html?ws={ws}"
    ))
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
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
    let payload_proxy = payload
        .as_ref()
        .and_then(|payload| trimmed_optional(payload.proxy.as_deref()));
    let query_proxy = trimmed_optional(query.proxy.as_deref()).or_else(|| {
        launch
            .as_ref()
            .and_then(|launch| trimmed_optional(launch.proxy.as_deref()))
    });
    let payload_extension_paths = payload
        .as_ref()
        .and_then(|payload| payload.extension_paths.clone())
        .map(clean_extension_paths)
        .unwrap_or_default();
    let query_extension_paths = parse_query_extension_paths(query.extension_paths.as_deref());
    let launch_extension_paths = launch
        .as_ref()
        .and_then(|launch| launch.extension_paths.clone())
        .map(clean_extension_paths)
        .unwrap_or_default();
    let extension_paths = if !payload_extension_paths.is_empty() {
        payload_extension_paths
    } else if !query_extension_paths.is_empty() {
        query_extension_paths
    } else {
        launch_extension_paths
    };
    let payload_profile_id = payload
        .as_ref()
        .and_then(|payload| trimmed_optional(payload.persistent_profile_id.as_deref()));
    let query_profile_id = trimmed_optional(query.persistent_profile_id.as_deref()).or_else(|| {
        launch
            .as_ref()
            .and_then(|launch| trimmed_optional(launch.persistent_profile_id.as_deref()))
    });

    if let Some(payload) = payload
        && let Some(args) = payload.extra_chrome_args
    {
        extra_chrome_args.extend(args);
    }

    Ok(SessionOverrides {
        stealth_enabled: payload_stealth.or(query_stealth),
        extra_chrome_args,
        proxy: payload_proxy.or(query_proxy),
        extension_paths,
        persistent_profile_id: payload_profile_id.or(query_profile_id),
    })
}

fn rest_session_overrides(
    query: &SessionQuery,
    proxy: Option<String>,
    extension_paths: Option<Vec<PathBuf>>,
    persistent_profile_id: Option<String>,
) -> Result<SessionOverrides, String> {
    let payload = if proxy.is_some() || extension_paths.is_some() || persistent_profile_id.is_some()
    {
        Some(CreateSessionRequest {
            stealth_enabled: None,
            extra_chrome_args: None,
            proxy,
            extension_paths,
            persistent_profile_id,
        })
    } else {
        None
    };

    session_overrides(query, payload)
}

fn parse_query_extension_paths(value: Option<&str>) -> Vec<PathBuf> {
    value
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

fn clean_extension_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths
        .into_iter()
        .filter(|path| !path.as_os_str().is_empty())
        .collect()
}

fn trimmed_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
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
        PoolError::PersistentProfilesDisabled | PoolError::InvalidPersistentProfileId(_) => {
            api_error(StatusCode::BAD_REQUEST, error.to_string())
        }
        PoolError::NotFound(_) => api_error(StatusCode::NOT_FOUND, error.to_string()),
        PoolError::NotConnectable(_) => api_error(StatusCode::CONFLICT, error.to_string()),
        PoolError::Browser(_) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
    }
}

fn rest_endpoint_error(error: RestEndpointError) -> Response {
    match error {
        RestEndpointError::BadRequest(error) => api_error(StatusCode::BAD_REQUEST, error),
        RestEndpointError::Pool(error) => pool_error(error),
        RestEndpointError::CdpTimeout(method) => {
            api_error(StatusCode::GATEWAY_TIMEOUT, format!("{method} timed out"))
        }
        RestEndpointError::CdpWebSocket(error) => api_error(
            StatusCode::BAD_GATEWAY,
            format!("CDP websocket failed: {error}"),
        ),
        RestEndpointError::CdpProtocol(error) => api_error(
            StatusCode::BAD_GATEWAY,
            format!("CDP command failed: {error}"),
        ),
        RestEndpointError::InvalidCdpResponse(error) => api_error(StatusCode::BAD_GATEWAY, error),
        RestEndpointError::Decode(error) => api_error(
            StatusCode::BAD_GATEWAY,
            format!("CDP base64 decode failed: {error}"),
        ),
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
    fn parses_browserless_style_launch_query() {
        let query = SessionQuery {
            token: None,
            stealth: None,
            launch: Some(
                r#"{"args":["--window-size=1280,720"],"stealth":false,"proxy":"http://proxy.local:8080","extensionPaths":["/tmp/eiger-extension"]}"#.to_owned(),
            ),
            proxy: None,
            extension_paths: None,
            persistent_profile_id: None,
        };

        let overrides = session_overrides(&query, None).expect("valid launch query");

        assert_eq!(overrides.stealth_enabled, Some(false));
        assert_eq!(overrides.extra_chrome_args, vec!["--window-size=1280,720"]);
        assert_eq!(overrides.proxy.as_deref(), Some("http://proxy.local:8080"));
        assert_eq!(
            overrides.extension_paths,
            vec![PathBuf::from("/tmp/eiger-extension")]
        );
    }

    #[test]
    fn query_stealth_overrides_launch_stealth() {
        let query = SessionQuery {
            token: None,
            stealth: Some(true),
            launch: Some(r#"{"stealth":false}"#.to_owned()),
            proxy: None,
            extension_paths: None,
            persistent_profile_id: None,
        };

        let overrides = session_overrides(&query, None).expect("valid launch query");

        assert_eq!(overrides.stealth_enabled, Some(true));
    }

    #[test]
    fn payload_proxy_overrides_query_proxy() {
        let query = SessionQuery {
            token: None,
            stealth: None,
            launch: None,
            proxy: Some("http://query-proxy.local:8080".to_owned()),
            extension_paths: None,
            persistent_profile_id: None,
        };
        let payload = CreateSessionRequest {
            stealth_enabled: None,
            extra_chrome_args: None,
            proxy: Some("http://payload-proxy.local:8080".to_owned()),
            extension_paths: None,
            persistent_profile_id: None,
        };

        let overrides = session_overrides(&query, Some(payload)).expect("valid overrides");

        assert_eq!(
            overrides.proxy.as_deref(),
            Some("http://payload-proxy.local:8080")
        );
    }

    #[test]
    fn query_extension_paths_are_comma_separated() {
        let query = SessionQuery {
            token: None,
            stealth: None,
            launch: None,
            proxy: None,
            extension_paths: Some("/opt/eiger/one, /opt/eiger/two".to_owned()),
            persistent_profile_id: None,
        };

        let overrides = session_overrides(&query, None).expect("valid overrides");

        assert_eq!(
            overrides.extension_paths,
            vec![
                PathBuf::from("/opt/eiger/one"),
                PathBuf::from("/opt/eiger/two")
            ]
        );
    }

    #[test]
    fn payload_profile_id_overrides_query_profile_id() {
        let query = SessionQuery {
            token: None,
            stealth: None,
            launch: None,
            proxy: None,
            extension_paths: None,
            persistent_profile_id: Some("query-profile".to_owned()),
        };
        let payload = CreateSessionRequest {
            stealth_enabled: None,
            extra_chrome_args: None,
            proxy: None,
            extension_paths: None,
            persistent_profile_id: Some("payload-profile".to_owned()),
        };

        let overrides = session_overrides(&query, Some(payload)).expect("valid overrides");

        assert_eq!(
            overrides.persistent_profile_id.as_deref(),
            Some("payload-profile")
        );
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
