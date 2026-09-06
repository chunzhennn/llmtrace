use super::*;
use crate::plugins::PluginManager;
use crate::trace::{TraceEvent, build_trace};

/// SQLx creates and drops a separate database for this test; never use live data.
#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL pointing to a local PostgreSQL test server"]
async fn enterprise_storage_round_trip(pool: PgPool) -> anyhow::Result<()> {
    let plugins = PluginManager::load(&[])?;
    let root = std::env::temp_dir().join(format!("llmtrace-archive-test-{}", Uuid::new_v4()));
    let request_body = serde_json::to_vec(&json!({
        "model":"test-model", "metadata":{"session_id":"conversation"},
        "messages":[{"role":"user","content":"word ".repeat(1_000_000)}]
    }))?;
    let response_body = br#"{"model":"test-model","choices":[{"message":{"content":null,"tool_calls":[{"id":"call-1","function":{"name":"search","arguments":"{}"}}]}}],"usage":{"prompt_tokens":1000000,"completion_tokens":10}}"#;
    for (index, backend) in [
        ArchiveStorageBackend::Postgres,
        ArchiveStorageBackend::Filesystem,
        ArchiveStorageBackend::Filesystem,
    ]
    .into_iter()
    .enumerate()
    {
        let archive = ArchiveConfig {
            storage_backend: backend,
            filesystem_root: if index == 2 {
                root.join("not-a-directory")
            } else {
                root.clone()
            },
            ..Default::default()
        };
        if index == 2 {
            std::fs::write(&archive.filesystem_root, b"force PostgreSQL fallback")?;
        }
        let mut ids = Vec::new();
        for turn in 0..2 {
            let id = Uuid::new_v4();
            let mut event = TraceEvent::base(id, Utc::now() - ChronoDuration::minutes(2 - turn));
            event.completed_at = Some(event.started_at + ChronoDuration::seconds(1));
            event.original_uri = "/v1/chat/completions".into();
            event.upstream_url = "https://llm.example/v1/chat/completions".into();
            event.api_key_hash = Some(format!("test-key-{index}"));
            event.status = Some(if turn == 0 { 429 } else { 200 });
            event.duration_ms = Some(1000);
            event.request_body = request_body.clone();
            event.response_body = response_body.to_vec();
            event.request_body_bytes = request_body.len() as i64;
            event.response_body_bytes = response_body.len() as i64;
            event.response_body_truncated = turn == 0;
            event.user_id = Some(format!("employee-{index}"));
            event.user_name = Some(format!("Employee {index}"));
            let (mut trace, messages, user_id, user_name) = build_trace(event, &plugins)?;
            if trace.usage_complete {
                trace.estimated_cost_microusd = Some(2_000_080);
            }
            insert_trace(&pool, &archive, trace, messages, user_id, user_name).await?;
            ids.push(id);
        }
        // Appending a second request must not change the first archive.
        for id in &ids {
            let detail = get_request(&pool, *id, &archive, true).await?.unwrap();
            assert_eq!(
                detail["request_body"].as_str().unwrap().as_bytes(),
                request_body
            );
            assert_eq!(
                detail["response_body"].as_str().unwrap().as_bytes(),
                response_body
            );
            assert_eq!(detail["tool_calls"][0]["name"], "search");
            assert_eq!(detail["input_tokens"], 1000000);
        }
        let detail = get_request(&pool, ids[1], &archive, false).await?.unwrap();
        assert_eq!(detail["request_body"], "");
        assert_eq!(detail["bodies_included"], false);
        let session_id = Uuid::parse_str(detail["session_id"].as_str().unwrap())?;
        let session = get_session(&pool, session_id, None, None).await?.unwrap();
        assert_eq!(session["request_stats"]["request_count"], 2);
        assert_eq!(session["request_stats"]["error_count"], 1);
        assert_eq!(session["request_stats"]["input_tokens"], 2000000);
        assert_eq!(session["request_stats"]["usage_known_count"], 1);
        assert_eq!(session["request_stats"]["priced_request_count"], 1);
        assert_eq!(session["request_stats"]["estimated_cost_microusd"], 2000080);
        assert_eq!(session["user_name"], format!("Employee {index}"));
        let incomplete: bool = sqlx::query_scalar("SELECT NOT complete FROM payload_archive_records WHERE trace_id = $1 AND direction = 'response_body'")
            .bind(ids[0]).fetch_one(&pool).await?;
        assert!(incomplete);
        let sealed: bool = sqlx::query_scalar(
            "SELECT bool_and(sealed) FROM payload_archive_segments WHERE session_id = $1",
        )
        .bind(session_id)
        .fetch_one(&pool)
        .await?;
        assert!(sealed);
    }
    assert_eq!(stats(&pool).await?["errors"], 3);
    let rows = list_requests(&pool, RequestListFilters::default()).await?;
    assert_eq!(rows["items"].as_array().unwrap().len(), 6);
    assert_eq!(rows["items"][0]["tool_call_count"], 1);
    let users = user_usage(&pool, None, None).await?;
    assert_eq!(users["items"].as_array().unwrap().len(), 3);
    assert_eq!(users["items"][0]["input_tokens"], 2000000);
    model_usage(&pool, None, None).await?;
    api_key_usage(&pool, None, None).await?;
    recent_error_requests(&pool, None, None).await?;
    slow_requests(&pool, None, None, None, None).await?;
    let query: StructuredQuery = serde_json::from_value(json!({
        "dataset":"requests", "fields":["input_tokens","estimated_cost_microusd","tool_call_count","ttfb_ms"],
        "filters":[{"field":"input_tokens","op":"gte","value":100000}]
    }))?;
    run_structured_query(&pool, query).await?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL and a local HTTP test server"]
async fn enterprise_proxy_stream_and_admin_access(pool: PgPool) -> anyhow::Result<()> {
    use crate::{config::Config, metrics::RuntimeMetrics, state::AppState, trace::TraceRecorder};
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode},
        routing::post,
    };
    use futures_util::StreamExt;
    use std::sync::Arc;
    use tower::ServiceExt;

    let chunks = vec![
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":5}}\n\ndata: [DONE]\n\n",
    ];
    let expected = chunks.concat();
    let upstream = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let chunks = chunks.clone();
            async move {
                let stream = futures_util::stream::iter(chunks).then(|chunk| async move {
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    Ok::<_, std::io::Error>(bytes::Bytes::from_static(chunk.as_bytes()))
                });
                (
                    [("content-type", "text/event-stream")],
                    Body::from_stream(stream),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, upstream).await });
    let mut config = Config::default();
    config.proxy.default_upstream = format!("http://{address}");
    config.proxy.allow_upstreams = vec![address.to_string()];
    config.archive.storage_backend = ArchiveStorageBackend::Postgres;
    config.pricing.insert(
        "test-model".into(),
        crate::pricing::ModelPrice {
            input: 2.0,
            output: 8.0,
            cache_read: None,
            cache_write: None,
        },
    );
    let plugins = Arc::new(PluginManager::load(&[])?);
    let metrics = RuntimeMetrics::default();
    let (recorder, pipeline) = TraceRecorder::spawn(
        pool.clone(),
        plugins.clone(),
        config.archive.clone(),
        Arc::new(config.pricing.clone()),
        &StorageConfig {
            trace_queue_capacity: 8,
            trace_worker_count: 1,
            journal: crate::config::JournalConfig {
                enabled: false,
                ..Default::default()
            },
            ..Default::default()
        },
        metrics.clone(),
    )?;
    let state = AppState::new(config, pool.clone(), plugins, recorder, metrics)?;
    let app = crate::build_router(state.clone());

    let methods = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/auth/methods")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(methods.status(), StatusCode::OK);
    let methods: Value =
        serde_json::from_slice(&axum::body::to_bytes(methods.into_body(), 1024).await?)?;
    assert_eq!(methods, json!({"local":true,"oauth":false}));

    let unauthorized = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/requests")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    let response = app.clone().oneshot(Request::builder().method("POST").uri("/v1/chat/completions")
        .header("content-type", "application/json").header("authorization", "Bearer employee-key")
        .body(Body::from(r#"{"model":"test-model","stream":true,"messages":[{"role":"user","content":"hi"}]}"#))?).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let id = Uuid::parse_str(response.headers()["x-llmtrace-trace-id"].to_str()?)?;
    let received = axum::body::to_bytes(response.into_body(), 100_000).await?;
    assert_eq!(received.as_ref(), expected.as_bytes());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM request_traces WHERE id = $1")
                    .bind(id)
                    .fetch_one(&pool)
                    .await?;
            if count == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    let login = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .header("origin", "http://127.0.0.1:3000")
                .extension(axum::extract::ConnectInfo(
                    "127.0.0.1:12345".parse::<std::net::SocketAddr>()?,
                ))
                .body(Body::from(r#"{"username":"admin","password":"admin"}"#))?,
        )
        .await?;
    assert_eq!(login.status(), StatusCode::OK);
    let cookie = login.headers()["set-cookie"]
        .to_str()?
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let detail = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/requests/{id}?include_bodies=false"))
                .header("cookie", &cookie)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(detail.status(), StatusCode::OK);
    let value: Value =
        serde_json::from_slice(&axum::body::to_bytes(detail.into_body(), 100_000).await?)?;
    assert_eq!(value["input_tokens"], 100);
    assert_eq!(value["output_tokens"], 5);
    assert_eq!(value["estimated_cost_microusd"], 240);
    assert_eq!(value["usage_complete"], true);
    assert_eq!(value["bodies_included"], false);
    assert!(value["error"].is_null());
    assert!(value["ttft_ms"].as_i64().unwrap() >= value["ttfb_ms"].as_i64().unwrap());
    let cross_origin = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/query")
                .header("cookie", cookie)
                .header("origin", "https://unrelated.example")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"dataset":"requests"}"#))?,
        )
        .await?;
    assert_eq!(cross_origin.status(), StatusCode::FORBIDDEN);
    drop(app);
    drop(state);
    pipeline.drain().await;
    server.abort();
    Ok(())
}
