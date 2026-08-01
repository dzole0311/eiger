use anyhow::Context;
use axum::{
    Router,
    http::{HeaderValue, Method, header},
};
use eiger_api::{ApiState, router};
use eiger_config::AppConfig;
use eiger_pool::SessionPool;
use tokio::net::TcpListener;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let config = AppConfig::from_env().context("failed to load configuration")?;
    let pool = SessionPool::from_config(&config);
    let _maintenance_task = pool.clone().spawn_maintenance();
    let app = server_layers(router(ApiState::new(pool, &config)), &config)?;

    let listener = TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("failed to bind {}", config.bind_addr))?;

    info!(addr = %config.bind_addr, "eiger server listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")
}

fn server_layers(app: Router, config: &AppConfig) -> anyhow::Result<Router> {
    let app = if config.http.cors_origins.is_empty() {
        app
    } else {
        app.layer(cors_layer(config)?)
    };

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

fn init_tracing() {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("eiger_server=info,eiger_api=info,eiger_pool=info"));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer())
        .init();
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
