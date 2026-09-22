mod api;
mod auth;
mod config;
mod health;
mod login_throttle;
mod metrics;
mod parsers;
mod plugins;
mod pricing;
mod proxy;
mod redaction;
mod routing;
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
use axum::serve::ListenerExt;
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
#[command(name = "llmtrace", version)]
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
    config.log_startup_warnings();
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
    let archive_maintenance = storage::spawn_archive_maintenance(
        pool.clone(),
        config.storage.clone(),
        config.archive.clone(),
        runtime_metrics.clone(),
    );
    let plugins = Arc::new(PluginManager::load(&config.plugins).context("failed to load plugins")?);
    let (recorder, pipeline) = TraceRecorder::spawn(
        pool.clone(),
        plugins.clone(),
        config.archive.clone(),
        Arc::new(config.pricing.clone()),
        &config.storage,
        runtime_metrics.clone(),
    )?;
    let state = AppState::new(config.clone(), pool, plugins, recorder, runtime_metrics)
        .context("failed to initialize app state")?;
    let server_result = serve_routers(&config, &state).await;

    if let Some(handle) = retention_pruner {
        handle.abort();
        let _ = handle.await;
    }
    archive_maintenance.abort();
    let _ = archive_maintenance.await;

    // Drop every recorder handle so the pipeline observes a closed channel, then drain it.
    drop(state);
    pipeline.drain().await;
    tracing::info!("shutdown complete");
    server_result
}

async fn serve_routers(config: &Config, state: &AppState) -> anyhow::Result<()> {
    // Bind both sockets before accepting traffic so a failed admin bind cannot
    // leave an apparently healthy, partially configured service running.
    let mut listeners = vec![(
        TcpListener::bind(&config.server.listen)
            .await
            .context("failed to bind proxy listener")?,
        build_router(state.clone()),
        if config.server.admin_listen.is_some() {
            "proxy"
        } else {
            "combined"
        },
    )];
    if let Some(addr) = &config.server.admin_listen {
        listeners.push((
            TcpListener::bind(addr)
                .await
                .context("failed to bind admin listener")?,
            build_admin_router(state.clone()),
            "admin",
        ));
    }
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let mut servers = tokio::task::JoinSet::new();
    for (listener, app, role) in listeners {
        tracing::info!(addr = %listener.local_addr()?, role, "llmtrace listening");
        let listener = listener.tap_io(|socket| {
            if let Err(error) = socket.set_nodelay(true) {
                tracing::warn!(%error, "failed to disable TCP buffering");
            }
        });
        let mut receiver = receiver.clone();
        servers.spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async move {
                let _ = receiver.wait_for(|stop| *stop).await;
            })
            .await
            .context("server failed")
        });
    }
    let first_result = tokio::select! {
        _ = shutdown_signal() => None,
        result = servers.join_next() => result,
    };
    let _ = shutdown.send(true);
    // Stop both listeners on signals or failure, then allow in-flight streams
    // to finish before main drops recorder handles and drains the journal.
    let mut result = first_result
        .map(|r| r.context("server task failed").and_then(|r| r))
        .unwrap_or(Ok(()));
    while let Some(next) = servers.join_next().await {
        let next = next.context("server task failed").and_then(|r| r);
        if result.is_ok() {
            result = next;
        }
    }
    result
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
    let router = if state.config.server.admin_listen.is_some() {
        Router::new()
    } else {
        platform_routes(&state)
    };
    router
        .fallback(proxy::proxy)
        .with_state(state)
        .layer(TraceLayer::new_for_http().make_span_with(request_trace_span))
}

fn build_admin_router(state: AppState) -> Router {
    platform_routes(&state)
        .with_state(state)
        .layer(TraceLayer::new_for_http().make_span_with(request_trace_span))
}

fn platform_routes(state: &AppState) -> Router<AppState> {
    let protected_api = api::router().route_layer(middleware::from_fn_with_state(
        state.clone(),
        auth::require_auth,
    ));
    let private_routes = Router::new()
        .nest("/api/auth", auth::router())
        .nest("/api", protected_api)
        .route("/", axum::routing::get(ui::redirect_to_ui))
        .route("/ui", axum::routing::get(ui::serve_ui_index))
        .route("/ui/", axum::routing::get(ui::serve_ui_index))
        .route("/ui/{*path}", axum::routing::get(ui::serve_ui_path))
        .layer(DefaultBodyLimit::max(PRIVATE_JSON_BODY_LIMIT_BYTES))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            private_response_headers,
        ));

    Router::new().merge(health::router()).merge(private_routes)
}

fn request_trace_span<B>(request: &Request<B>) -> tracing::Span {
    tracing::info_span!(
        "request",
        method = %request.method(),
        path = request_trace_path(request),
        version = ?request.version(),
    )
}

fn request_trace_path<B>(request: &Request<B>) -> &str {
    request.uri().path()
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
    fn request_trace_path_excludes_query_values() {
        let request = Request::builder()
            .uri("/v1/messages?api_key=sk-secret&debug=true")
            .body(Body::empty())
            .unwrap();

        assert_eq!(request_trace_path(&request), "/v1/messages");
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
