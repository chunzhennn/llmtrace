//! Synthetic gateway tests; SQLx owns an isolated database and no provider calls occur.
use super::*;
use crate::{
    config::{Config, ProxyPreset},
    plugins::PluginManager,
    state::AppState,
    trace::TraceRecorder,
};
use axum::{
    Router,
    body::Body,
    extract::ws::WebSocketUpgrade,
    http::{Request, StatusCode},
    response::IntoResponse,
    routing::get,
};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tower::ServiceExt;

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL and local sockets; uses only synthetic upstream traffic"]
async fn litellm_split_listeners_stream_and_isolate_admin(pool: PgPool) -> anyhow::Result<()> {
    let release_download = Arc::new(tokio::sync::Notify::new());
    let gate = release_download.clone();
    let upstream = Router::new()
        .route("/v1/realtime", get(|ws: WebSocketUpgrade| async {
            ws.on_upgrade(|mut socket| async move {
                while let Some(Ok(message)) = socket.recv().await {
                    if socket.send(message).await.is_err() { break; }
                }
            })
        }))
        .route("/files/large/content", get(move || {
            let gate = gate.clone();
            async move {
                let chunks = futures_util::stream::unfold(0, move |index| {
                    let gate = gate.clone();
                    async move {
                        if index == 8 { return None; }
                        if index == 1 { gate.notified().await; }
                        Some((Ok::<_, io::Error>(Bytes::from(vec![b'x'; 256 * 1024])), index + 1))
                    }
                });
                ([("content-type", "application/octet-stream")], Body::from_stream(chunks))
            }
        }))
        .fallback(|request: Request<Body>| async move {
            let (parts, body) = request.into_parts();
            let body = axum::body::to_bytes(body, 1024 * 1024).await.unwrap();
            if parts.uri.path().ends_with("chat/completions") && parts.method == "POST" {
                let chunks = futures_util::stream::iter([
                    "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
                    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1}}\n\ndata: [DONE]\n\n",
                ]).map(|chunk| Ok::<_, io::Error>(Bytes::from_static(chunk.as_bytes())));
                return ([("content-type", "text/event-stream")], Body::from_stream(chunks)).into_response();
            }
            let value = json!({
                "upstream": true, "uri": parts.uri.to_string(), "method": parts.method.as_str(),
                "authorization": parts.headers.get("authorization").and_then(|h| h.to_str().ok()),
                "body": String::from_utf8_lossy(&body),
            });
            (
                if parts.uri.path() == "/files/error" { StatusCode::TOO_MANY_REQUESTS } else { StatusCode::OK },
                [("x-upstream", "litellm-fixture"), ("retry-after", "2"), ("access-control-allow-origin", "https://employee.example")],
                axum::Json(value),
            ).into_response()
        });
    let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let upstream_addr = upstream_listener.local_addr()?;
    let upstream_task = tokio::spawn(async move { axum::serve(upstream_listener, upstream).await });

    let proxy_reservation = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let admin_reservation = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let proxy_addr = proxy_reservation.local_addr()?;
    let admin_addr = admin_reservation.local_addr()?;
    let proxy_url = format!("http://{proxy_addr}");
    let admin_url = format!("http://localhost:{}", admin_addr.port());
    let mut config = Config::default();
    config.server.listen = proxy_addr.to_string();
    config.server.admin_listen = Some(admin_addr.to_string());
    config.server.public_url = admin_url.clone();
    config.server.proxy_public_url = Some(proxy_url.clone());
    config.proxy.preset = ProxyPreset::Litellm;
    config.proxy.default_upstream = format!("http://{upstream_addr}");
    config.proxy.allow_upstreams = vec![
        format!("http://{upstream_addr}"),
        format!("ws://{upstream_addr}"),
    ];
    config.proxy.max_request_body_bytes = 1024;
    config.archive.storage_backend = ArchiveStorageBackend::Postgres;
    config.storage.journal.enabled = false;
    config.validate()?;
    let plugins = Arc::new(PluginManager::load(&[])?);
    let metrics = RuntimeMetrics::default();
    let (recorder, pipeline) = TraceRecorder::spawn(
        pool.clone(),
        plugins.clone(),
        config.archive.clone(),
        Arc::new(config.pricing.clone()),
        &config.storage,
        metrics.clone(),
    )?;
    let state = AppState::new(config.clone(), pool.clone(), plugins, recorder, metrics)?;
    drop(proxy_reservation);
    drop(admin_reservation);
    let running_state = state.clone();
    let server = tokio::spawn(async move { crate::serve_routers(&config, &running_state).await });
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if http
                .get(format!("{admin_url}/readyz"))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;

    // Preset forwarding preserves query strings, employee keys, error codes and CORS.
    for path in [
        "/models?team=engineering",
        "/v1/models",
        "/key/info",
        "/health/readiness",
        "/files/error",
    ] {
        let response = http
            .get(format!("{proxy_url}{path}"))
            .bearer_auth("employee-key")
            .send()
            .await?;
        assert_eq!(
            response.status(),
            if path == "/files/error" {
                StatusCode::TOO_MANY_REQUESTS
            } else {
                StatusCode::OK
            }
        );
        assert_eq!(response.headers()["x-upstream"], "litellm-fixture");
        assert_eq!(response.headers()["retry-after"], "2");
        assert!(!response.headers().contains_key("x-llmtrace-trace-id"));
        let value: Value = response.json().await?;
        assert_eq!(value["authorization"], "Bearer employee-key");
        assert_eq!(value["uri"], path);
    }
    let options = http
        .request(
            reqwest::Method::OPTIONS,
            format!("{proxy_url}/v1/chat/completions"),
        )
        .send()
        .await?;
    assert_eq!(
        options.headers()["access-control-allow-origin"],
        "https://employee.example"
    );
    assert!(!options.headers().contains_key("x-llmtrace-trace-id"));
    assert_eq!(
        http.post(format!("{proxy_url}/files"))
            .body(vec![b'x'; 1025])
            .send()
            .await?
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let chunked = reqwest::Body::wrap_stream(futures_util::stream::iter([Ok::<_, io::Error>(
        Bytes::from(vec![b'x'; 1025]),
    )]));
    assert_eq!(
        http.post(format!("{proxy_url}/files"))
            .body(chunked)
            .send()
            .await?
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        http.get(format!("{proxy_url}/models"))
            .header("x-llmtrace-upstream", "http://not-allowed.invalid")
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );

    for path in [
        "/api/auth/methods",
        "/api/requests",
        "/ui",
        "/healthz",
        "/metrics",
        "/key/generate",
    ] {
        assert_eq!(
            http.get(format!("{proxy_url}{path}"))
                .header("host", "localhost")
                .header("x-forwarded-host", "localhost")
                .send()
                .await?
                .status(),
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    assert_eq!(
        http.get(format!("{admin_url}/v1/models"))
            .send()
            .await?
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        http.get(format!("{admin_url}/api/requests"))
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        http.get(format!("{admin_url}/ui")).send().await?.status(),
        StatusCode::OK
    );
    let credentials = json!({"username":"admin","password":"admin"});
    let login_url = format!("{admin_url}/api/auth/login");
    assert_eq!(
        http.post(&login_url)
            .header("origin", &proxy_url)
            .json(&credentials)
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    let login = http
        .post(&login_url)
        .header("origin", &admin_url)
        .json(&credentials)
        .send()
        .await?;
    assert_eq!(login.status(), StatusCode::OK);
    let cookie_header = login.headers()["set-cookie"].to_str()?;
    assert!(!cookie_header.to_lowercase().contains("domain="));
    let cookie = cookie_header.split(';').next().unwrap().to_string();
    assert_eq!(
        http.get(format!("{admin_url}/api/requests"))
            .header("cookie", &cookie)
            .send()
            .await?
            .status(),
        StatusCode::OK
    );

    // A blocked producer proves supporting downloads reach the client incrementally.
    let mut download = http
        .get(format!("{proxy_url}/files/large/content"))
        .send()
        .await?
        .bytes_stream();
    let first = tokio::time::timeout(Duration::from_secs(1), download.next())
        .await?
        .unwrap()?;
    assert!(!first.is_empty());
    release_download.notify_one();
    let mut total = first.len();
    while let Some(chunk) = download.next().await {
        total += chunk?.len();
    }
    assert_eq!(total, 2 * 1024 * 1024);
    let mut concurrent = futures_util::stream::iter(0..100)
        .map(|_| {
            let http = http.clone();
            let url = format!("{proxy_url}/models");
            async move {
                http.get(url)
                    .send()
                    .await?
                    .error_for_status()?
                    .bytes()
                    .await
            }
        })
        .buffer_unordered(16);
    while let Some(result) = concurrent.next().await {
        result?;
    }

    // Even when all upstream paths are enabled, llmtrace admin routes stay isolated.
    let mut all_config = (*state.config).clone();
    all_config.proxy.path_prefixes = Some(vec!["/".into()]);
    let all_state = AppState::new(
        all_config,
        pool.clone(),
        state.plugins.clone(),
        state.traces.clone(),
        state.runtime_metrics.clone(),
    )?;
    let all_routes = crate::build_router(all_state.clone());
    for path in [
        "/api/requests",
        "/api/auth/methods",
        "/ui",
        "/healthz",
        "/metrics",
        "/",
    ] {
        let response = all_routes
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty())?)
            .await?;
        assert_eq!(
            response.headers()["x-upstream"],
            "litellm-fixture",
            "{path}"
        );
        assert!(!response.headers().contains_key("x-llmtrace-trace-id"));
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_traces")
        .fetch_one(&pool)
        .await?;
    assert_eq!(count, 0, "supporting traffic must not create traces");

    let redirected = http
        .post(format!("{proxy_url}/models"))
        .header(
            "x-llmtrace-upstream",
            format!("http://{upstream_addr}/v1/chat/completions"),
        )
        .body("{}")
        .send()
        .await?;
    assert_eq!(
        redirected.json::<Value>().await?["uri"],
        "/v1/chat/completions/models"
    );
    for path in [
        "/models/../chat/completions",
        "/v1/%2e%2e/key/generate",
        "/models/a%2fb",
    ] {
        // oneshot preserves dot segments that URL-based HTTP clients normalize.
        let response = all_routes
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty())?)
            .await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
    }

    for path in ["/chat/completions", "/v1/chat/completions"] {
        let response = http.post(format!("{proxy_url}{path}")).bearer_auth("employee-key").json(&json!({"model":"fixture", "stream":true, "messages":[{"role":"user","content":"hello"}]})).send().await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().contains_key("x-llmtrace-trace-id"));
        assert!(response.text().await?.ends_with("data: [DONE]\n\n"));
    }
    let ws_url = format!("ws://{proxy_addr}/v1/realtime?model=fixture");
    let (mut socket, response) = tokio_tungstenite::connect_async(ws_url).await?;
    assert!(response.headers().contains_key("x-llmtrace-trace-id"));
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            "hello".into(),
        ))
        .await?;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await?
            .unwrap()?
            .into_text()?,
        "hello"
    );
    socket.close(None).await?;
    drop(socket);

    let mut no_capture_config = (*state.config).clone();
    no_capture_config.proxy.capture_path_prefixes = Some(vec![]);
    let no_capture_state = AppState::new(
        no_capture_config,
        pool.clone(),
        state.plugins.clone(),
        state.traces.clone(),
        state.runtime_metrics.clone(),
    )?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let ws_url = format!("ws://{}/v1/realtime", listener.local_addr()?);
    let no_capture_server =
        tokio::spawn(
            async move { axum::serve(listener, crate::build_router(no_capture_state)).await },
        );
    let (mut socket, response) = tokio_tungstenite::connect_async(ws_url).await?;
    assert!(!response.headers().contains_key("x-llmtrace-trace-id"));
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            "unrecorded".into(),
        ))
        .await?;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await?
            .unwrap()?
            .into_text()?,
        "unrecorded"
    );
    socket.close(None).await?;
    drop(socket);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_traces")
                .fetch_one(&pool)
                .await?;
            if count == 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        anyhow::Ok(())
    })
    .await??;
    server.abort();
    let _ = server.await;
    upstream_task.abort();
    let _ = upstream_task.await;
    no_capture_server.abort();
    let _ = no_capture_server.await;
    drop(concurrent);
    drop(all_routes);
    drop(all_state);
    drop(state);
    pipeline.drain().await;
    let (traces, sessions): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM request_traces), (SELECT COUNT(*) FROM trace_sessions)",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!((traces, sessions), (3, 3));
    Ok(())
}
