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
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderValue, Request, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use clap::Parser;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::metrics::RuntimeMetrics;
use crate::plugins::PluginManager;
use crate::state::AppState;
use crate::trace::TraceRecorder;

const PRIVATE_JSON_BODY_LIMIT_BYTES: usize = 256 * 1024;
const STRICT_TRANSPORT_SECURITY_VALUE: &str = "max-age=31536000";

#[derive(Parser, Debug)]
#[command(name = "llmtrace")]
#[command(about = "Application-layer LLM reverse proxy with tracing")]
struct Args {
    #[arg(short, long, env = "LLMTRACE_CONFIG")]
    config: Option<PathBuf>,

    #[arg(long, conflicts_with = "migrate_only")]
    check_config: bool,

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
    if args.check_config {
        tracing::info!("config is valid");
        return Ok(());
    }

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
    let private_routes = Router::new()
        .nest("/api/auth", auth::router())
        .nest("/api", protected_api)
        .route("/", axum::routing::get(ui::redirect_to_ui))
        .route("/ui", axum::routing::get(ui::serve_ui))
        .route("/ui/", axum::routing::get(ui::serve_ui))
        .route("/ui/{*path}", axum::routing::get(ui::serve_ui))
        .layer(DefaultBodyLimit::max(PRIVATE_JSON_BODY_LIMIT_BYTES))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            private_response_headers,
        ));

    Router::new()
        .merge(health::router())
        .merge(private_routes)
        .fallback(proxy::proxy)
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

async fn private_response_headers(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let mut response = next.run(request).await;
    set_private_response_headers(
        response.headers_mut(),
        private_response_hsts_enabled(&state.config),
    );
    response
}

fn private_response_hsts_enabled(config: &Config) -> bool {
    config.auth.cookie_secure
        && url::Url::parse(&config.server.public_url).is_ok_and(|url| url.scheme() == "https")
}

fn set_private_response_headers(headers: &mut HeaderMap, hsts_enabled: bool) {
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    headers.insert(header::EXPIRES, HeaderValue::from_static("0"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    if hsts_enabled {
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static(STRICT_TRANSPORT_SECURITY_VALUE),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Json;
    use axum::http::StatusCode;
    use axum::routing::post;
    use tower::ServiceExt;

    #[test]
    fn args_parse_check_config_mode() {
        let args = Args::try_parse_from(["llmtrace", "--check-config"]).unwrap();

        assert!(args.check_config);
        assert!(!args.migrate_only);
    }

    #[test]
    fn args_reject_check_config_with_migrate_only() {
        let error = Args::try_parse_from(["llmtrace", "--check-config", "--migrate-only"])
            .unwrap_err()
            .to_string();

        assert!(error.contains("cannot be used with"));
    }

    #[test]
    fn private_response_headers_disable_caching_and_browser_sniffing() {
        let mut headers = HeaderMap::new();

        set_private_response_headers(&mut headers, false);

        assert_eq!(
            header_value(&headers, header::CACHE_CONTROL),
            Some("no-store")
        );
        assert_eq!(header_value(&headers, header::PRAGMA), Some("no-cache"));
        assert_eq!(header_value(&headers, header::EXPIRES), Some("0"));
        assert_eq!(
            header_value(&headers, header::X_CONTENT_TYPE_OPTIONS),
            Some("nosniff")
        );
        assert_eq!(
            header_value(&headers, header::REFERRER_POLICY),
            Some("no-referrer")
        );
        assert_eq!(
            header_value(&headers, header::X_FRAME_OPTIONS),
            Some("DENY")
        );
        assert_eq!(
            header_value(&headers, header::STRICT_TRANSPORT_SECURITY),
            None
        );
    }

    #[test]
    fn private_response_headers_add_hsts_when_enabled() {
        let mut headers = HeaderMap::new();

        set_private_response_headers(&mut headers, true);

        assert_eq!(
            header_value(&headers, header::STRICT_TRANSPORT_SECURITY),
            Some(STRICT_TRANSPORT_SECURITY_VALUE)
        );
    }

    #[test]
    fn private_response_hsts_requires_https_public_url_and_secure_cookie() {
        let mut config = Config::default();

        assert!(!private_response_hsts_enabled(&config));

        config.auth.cookie_secure = true;
        assert!(!private_response_hsts_enabled(&config));

        config.server.public_url = "https://llmtrace.example.com".to_string();
        assert!(private_response_hsts_enabled(&config));
    }

    #[tokio::test]
    async fn private_json_body_limit_allows_small_json_payloads() {
        let app = json_echo_router();

        let response = app
            .oneshot(json_request(Body::from(r#"{"ok":true}"#)))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn private_json_body_limit_rejects_oversized_json_payloads() {
        let app = json_echo_router();
        let body = format!(
            r#"{{"data":"{}"}}"#,
            "x".repeat(PRIVATE_JSON_BODY_LIMIT_BYTES)
        );

        let response = app.oneshot(json_request(Body::from(body))).await.unwrap();

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    fn json_echo_router() -> Router {
        Router::new()
            .route(
                "/json",
                post(|Json(_payload): Json<serde_json::Value>| async { StatusCode::OK }),
            )
            .layer(DefaultBodyLimit::max(PRIVATE_JSON_BODY_LIMIT_BYTES))
    }

    fn json_request(body: Body) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/json")
            .header(header::CONTENT_TYPE, "application/json")
            .body(body)
            .unwrap()
    }

    fn header_value(headers: &HeaderMap, name: axum::http::HeaderName) -> Option<&str> {
        headers.get(name).and_then(|value| value.to_str().ok())
    }
}
