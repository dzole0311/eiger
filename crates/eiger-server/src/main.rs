use std::{net::SocketAddr, time::Duration};

use anyhow::Context;
use axum::{
    Router,
    body::Body,
    http::{HeaderValue, Method, Request, Response, StatusCode, Uri, header},
};
use eiger_api::{ApiState, api_router, liveness_router};
use eiger_config::AppConfig;
use eiger_pool::SessionPool;
use tokio::net::TcpListener;
use tower_governor::{
    GovernorError, GovernorLayer, governor::GovernorConfigBuilder, key_extractor::KeyExtractor,
};
use tower_http::{cors::CorsLayer, limit::RequestBodyLimitLayer, trace::TraceLayer};
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let config = AppConfig::from_env().context("failed to load configuration")?;
    let pool = SessionPool::from_config(&config);
    let _maintenance_task = pool.clone().spawn_maintenance();
    let state = ApiState::new(pool, &config);
    let app = server_layers(state, &config)?;

    let listener = TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("failed to bind {}", config.bind_addr))?;

    info!(addr = %config.bind_addr, "eiger server listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("server error")
}

fn server_layers(state: ApiState, config: &AppConfig) -> anyhow::Result<Router> {
    let api = add_rate_limit_layers(api_router(state.clone()), config)?;
    let app = liveness_router(state).merge(api);
    let app = if config.http.cors_origins.is_empty() {
        app
    } else {
        app.layer(cors_layer(config)?)
    };
    let app = app.layer(RequestBodyLimitLayer::new(
        config.http.request_body_limit_bytes,
    ));

    Ok(app.layer(TraceLayer::new_for_http()))
}

fn cors_layer(config: &AppConfig) -> anyhow::Result<CorsLayer> {
    let origins = config
        .http
        .cors_origins
        .iter()
        .map(|origin| {
            HeaderValue::from_str(origin)
                .with_context(|| format!("invalid EIGER_CORS_ORIGINS origin: {origin}"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    Ok(CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([Method::GET, Method::POST, Method::DELETE])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]))
}

#[derive(Clone, Debug)]
struct TokenKeyExtractor;

impl KeyExtractor for TokenKeyExtractor {
    type Key = String;

    fn extract<T>(&self, req: &Request<T>) -> Result<Self::Key, GovernorError> {
        Ok(rate_limit_token(req.headers(), req.uri()).unwrap_or_else(|| "missing".to_owned()))
    }
}

fn add_rate_limit_layers(app: Router, config: &AppConfig) -> anyhow::Result<Router> {
    let mut peer_ip_builder = GovernorConfigBuilder::default();
    peer_ip_builder
        .period(rate_limit_period(config.rate_limit.requests_per_second))
        .burst_size(config.rate_limit.burst);
    let mut peer_ip_builder = peer_ip_builder.use_headers();
    let peer_ip_config = peer_ip_builder
        .finish()
        .context("invalid rate limit config")?;
    let app = app.layer(GovernorLayer::new(peer_ip_config).error_handler(rate_limit_error));

    if config.auth.token.is_some() {
        let mut token_builder = GovernorConfigBuilder::default();
        token_builder
            .period(rate_limit_period(config.rate_limit.requests_per_second))
            .burst_size(config.rate_limit.burst);
        let mut token_builder = token_builder.key_extractor(TokenKeyExtractor).use_headers();
        let token_config = token_builder
            .finish()
            .context("invalid rate limit config")?;

        Ok(app.layer(GovernorLayer::new(token_config).error_handler(rate_limit_error)))
    } else {
        Ok(app)
    }
}

fn rate_limit_period(requests_per_second: u32) -> Duration {
    let nanos = 1_000_000_000_u64 / u64::from(requests_per_second);
    Duration::from_nanos(nanos.max(1))
}

fn rate_limit_error(error: GovernorError) -> Response<Body> {
    let (status, message, headers) = match error {
        GovernorError::TooManyRequests { wait_time, headers } => (
            StatusCode::TOO_MANY_REQUESTS,
            format!("rate limit exceeded; retry after {wait_time} seconds"),
            headers,
        ),
        GovernorError::UnableToExtractKey => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "rate limit key could not be extracted".to_owned(),
            None,
        ),
        GovernorError::Other { code, msg, headers } => (
            code,
            msg.unwrap_or_else(|| "rate limit error".to_owned()),
            headers,
        ),
    };
    let body = format!(r#"{{"error":"{message}"}}"#);
    let mut response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .expect("valid rate limit response");

    if let Some(headers) = headers {
        response.headers_mut().extend(headers);
    }

    response
}

fn rate_limit_token(headers: &axum::http::HeaderMap, uri: &Uri) -> Option<String> {
    bearer_token(headers).or_else(|| query_token(uri))
}

fn bearer_token(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
}

fn query_token(uri: &Uri) -> Option<String> {
    uri.query()?.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key == "token" && !value.trim().is_empty() {
            urlencoding::decode(value)
                .ok()
                .map(|decoded| decoded.into_owned())
        } else {
            None
        }
    })
}

fn init_tracing() {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("eiger_server=info,eiger_api=info,eiger_pool=info"));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer())
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_period_converts_rps_to_interval() {
        assert_eq!(rate_limit_period(10), Duration::from_millis(100));
    }

    #[test]
    fn token_key_accepts_bearer_or_query_token() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer secret".parse().unwrap());

        assert_eq!(
            rate_limit_token(&headers, &"/sessions".parse().unwrap()),
            Some("secret".to_owned())
        );

        assert_eq!(
            rate_limit_token(
                &axum::http::HeaderMap::new(),
                &"/sessions?token=query%20secret".parse().unwrap()
            ),
            Some("query secret".to_owned())
        );
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
