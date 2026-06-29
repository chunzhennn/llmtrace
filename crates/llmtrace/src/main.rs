mod api;
mod auth;
mod config;
mod health;
mod login_throttle;
mod metrics;
mod parsers;
mod plugins;
mod proxy;
mod redaction;
mod state;
mod storage;
mod trace;
mod types;
mod ui;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use axum::Router;
use axum::middleware;
use clap::Parser;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::metrics::RuntimeMetrics;
use crate::plugins::PluginManager;
use crate::state::AppState;
use crate::trace::TraceRecorder;

#[derive(Parser, Debug)]
#[command(name = "llmtrace")]
#[command(about = "Application-layer LLM reverse proxy with tracing")]
struct Args {
    #[arg(short, long, env = "LLMTRACE_CONFIG")]
    config: Option<PathBuf>,

    #[arg(long)]
    migrate_only: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("llmtrace=info,tower_http=info,axum::rejection=debug")
        }))
        .init();

    let args = Args::parse();
    let config = Config::load(args.config.as_deref()).context("failed to load config")?;
    config.validate().context("invalid config")?;
    let pool = storage::connect(&config.storage)
        .await
        .context("failed to connect to postgres")?;
    storage::migrate(&pool)
        .await
        .context("failed to migrate database")?;

    if args.migrate_only {
        tracing::info!("migrations completed");
        return Ok(());
    }

    let runtime_metrics = RuntimeMetrics::default();
    let retention_pruner = storage::spawn_retention_pruner(
        pool.clone(),
        config.storage.clone(),
        runtime_metrics.clone(),
    );
    let plugins = Arc::new(PluginManager::load(&config.plugins).context("failed to load plugins")?);
    let (recorder, pipeline) = TraceRecorder::spawn(
        pool.clone(),
        plugins.clone(),
        config.redaction.body_redaction,
        config.storage.trace_queue_capacity,
        config.storage.trace_worker_count,
        runtime_metrics.clone(),
    );
    let state = AppState::new(config.clone(), pool, plugins, recorder, runtime_metrics)
        .context("failed to initialize app state")?;
    let app = build_router(state.clone());
    let addr: SocketAddr = config
        .server
        .listen
        .parse()
        .with_context(|| format!("invalid listen address {}", config.server.listen))?;
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;

    tracing::info!(%addr, "llmtrace listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("server failed")?;

    if let Some(handle) = retention_pruner {
        handle.abort();
        let _ = handle.await;
    }

    // Drop every recorder handle so the pipeline observes a closed channel, then drain it.
    drop(state);
    pipeline.drain().await;
    tracing::info!("shutdown complete");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => tracing::warn!(%error, "failed to install SIGTERM handler"),
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("shutdown signal received");
}

fn build_router(state: AppState) -> Router {
    let protected_api = api::router().route_layer(middleware::from_fn_with_state(
        state.clone(),
        auth::require_auth,
    ));

    Router::new()
        .merge(health::router())
        .nest("/api/auth", auth::router())
        .nest("/api", protected_api)
        .route("/", axum::routing::get(ui::redirect_to_ui))
        .route("/ui", axum::routing::get(ui::serve_ui))
        .route("/ui/", axum::routing::get(ui::serve_ui))
        .route("/ui/{*path}", axum::routing::get(ui::serve_ui))
        .fallback(proxy::proxy)
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}
