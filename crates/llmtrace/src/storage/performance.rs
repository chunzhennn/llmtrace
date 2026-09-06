//! Opt-in local benchmarks. SQLx creates an isolated database; all traffic is synthetic.
//! Build the release binary first, then run the ignored `performance_end_to_end` test.
use super::*;
use axum::serve::ListenerExt;
use axum::{
    Router,
    body::Body,
    extract::State,
    http::HeaderMap,
    response::IntoResponse,
    routing::{get, post},
};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt, stream};
use rand::{Rng, SeedableRng, distributions::Alphanumeric};
use std::collections::BTreeMap;
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Instant;

mod ui_review;

struct Server {
    child: Child,
    base: String,
    root: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    async fn start(
        pool: &PgPool,
        upstream: &str,
        backend: ArchiveStorageBackend,
        rotation: u64,
        slow_plugin: bool,
    ) -> anyhow::Result<Self> {
        let root = std::env::temp_dir().join(format!("llmtrace-perf-{}", Uuid::new_v4()));
        fs::create_dir_all(&root)?;
        let mut config = crate::config::Config::default();
        let socket = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = socket.local_addr()?;
        config.server.listen = address.to_string();
        config.server.public_url = format!("http://{address}");
        config.proxy.default_upstream = upstream.into();
        config.proxy.max_request_body_bytes = 16 * 1024 * 1024;
        config.proxy.max_response_body_bytes = 16 * 1024 * 1024;
        config.storage.trace_queue_capacity = 1024;
        config.storage.journal.directory = root.join("journal");
        config.storage.rotate_size_bytes = rotation;
        config.storage.rotate_check_interval_secs = 1;
        config.archive.storage_backend = backend;
        config.archive.filesystem_root = root.join("archive");
        if slow_plugin {
            let wasm_path = root.join("slow.wat");
            fs::write(
                &wasm_path,
                r#"(module
                (memory (export "memory") 1)
                (func (export "llmtrace_alloc") (param i32) (result i32) i32.const 1024)
                (func (export "llmtrace_on_request_start") (param i32 i32) (result i64)
                    (loop $spin (br $spin)) i64.const 0))"#,
            )?;
            config.plugins.push(crate::config::PluginConfig {
                name: "slow".into(),
                wasm_path,
                hooks: vec![crate::types::PluginHook::RequestStart],
                timeout_ms: 100,
                ..Default::default()
            });
        }
        let database: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(pool)
            .await?;
        let mut db_url = url::Url::parse(&std::env::var("DATABASE_URL")?)?;
        db_url.set_path(&database);
        config.storage.postgres_url = db_url.to_string();
        fs::write(root.join("config.toml"), toml::to_string(&config)?)?;
        let binary = std::env::var_os("LLMTRACE_PERF_BINARY")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/release/llmtrace")
            });
        anyhow::ensure!(
            binary.is_file(),
            "build the release binary before running benchmarks"
        );
        let mut command = Command::new(binary);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("LLMTRACE_") || key == "DATABASE_URL" {
                command.env_remove(key);
            }
        }
        command
            .args(["--config"])
            .arg(root.join("config.toml"))
            .env("RUST_LOG", "warn")
            .stdout(Stdio::from(fs::File::create(root.join("server.log"))?))
            .stderr(Stdio::from(fs::File::create(
                root.join("server-errors.log"),
            )?));
        drop(socket);
        let mut server = Self {
            child: command.spawn()?,
            base: config.server.public_url,
            root,
        };
        let client = reqwest::Client::new();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if client
                .get(format!("{}/readyz", server.base))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                return Ok(server);
            }
            anyhow::ensure!(
                server.child.try_wait()?.is_none(),
                "server exited; inspect {}",
                server.root.display()
            );
            anyhow::ensure!(
                Instant::now() < deadline,
                "server startup timed out; inspect {}",
                server.root.display()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn metrics(&self, client: &reqwest::Client) -> anyhow::Result<BTreeMap<String, f64>> {
        let text = client
            .get(format!("{}/metrics", self.base))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        Ok(text
            .lines()
            .filter(|s| !s.starts_with('#'))
            .filter_map(|line| {
                let (name, value) = line.split_once(' ')?;
                Some((name.to_string(), value.parse().ok()?))
            })
            .collect())
    }

    async fn settle(&self, client: &reqwest::Client) -> anyhow::Result<f64> {
        let started = Instant::now();
        loop {
            let metrics = self.metrics(client).await?;
            let journal_enabled = metrics
                .get("llmtrace_journal_enabled")
                .copied()
                .unwrap_or_default()
                > 0.0;
            let settled = if journal_enabled {
                metrics
                    .get("llmtrace_journal_pending_records")
                    .copied()
                    .unwrap_or_default()
                    == 0.0
                    && metric(&metrics, "queue_depth") == 0.0
                    && metric(&metrics, "memory_used_bytes") == 0.0
            } else {
                metric(&metrics, "enqueued_total")
                    == metric(&metrics, "persisted_total")
                        + metric(&metrics, "persist_failures_total")
                        + metric(&metrics, "build_failures_total")
            };
            if settled {
                return Ok(started.elapsed().as_secs_f64());
            }
            anyhow::ensure!(
                started.elapsed() < Duration::from_secs(120),
                "pipeline did not settle: {metrics:?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn login(&self, client: &reqwest::Client) -> anyhow::Result<String> {
        let response = client
            .post(format!("{}/api/auth/login", self.base))
            .json(&json!({"username":"admin", "password":"admin"}))
            .send()
            .await?
            .error_for_status()?;
        Ok(response.headers()["set-cookie"]
            .to_str()?
            .split(';')
            .next()
            .unwrap()
            .into())
    }
}

fn metric(values: &BTreeMap<String, f64>, name: &str) -> f64 {
    values
        .get(&format!("llmtrace_trace_pipeline_{name}"))
        .copied()
        .unwrap_or_default()
}

fn rss_bytes(pid: u32) -> u64 {
    fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|line| line.starts_with("VmRSS:"))
                .and_then(|line| line.split_whitespace().nth(1)?.parse::<u64>().ok())
        })
        .unwrap_or_default()
        * 1024
}

fn percentile(values: &[f64], fraction: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[((sorted.len() - 1) as f64 * fraction).ceil() as usize]
}

async fn mock_response(
    State(large): State<Bytes>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if headers.contains_key("x-test-stream") {
        let chunks = (0..12).map(|index| if index == 11 {
            Bytes::from_static(b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":10}}\n\ndata: [DONE]\n\n")
        } else {
            Bytes::from_static(b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n")
        });
        let stream = stream::iter(chunks).then(|chunk| async move {
            tokio::time::sleep(Duration::from_millis(2)).await;
            Ok::<_, io::Error>(chunk)
        });
        return (
            [("content-type", "text/event-stream")],
            Body::from_stream(stream),
        )
            .into_response();
    }
    let _ = body;
    let body = if headers.contains_key("x-test-large-response") {
        large
    } else {
        Bytes::from_static(br#"{"model":"test","choices":[{"message":{"role":"assistant","content":"hello"}}],"usage":{"prompt_tokens":10,"completion_tokens":1}}"#)
    };
    ([("content-type", "application/json")], body).into_response()
}

async fn mock_websocket(ws: axum::extract::ws::WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(|mut socket| async move {
        while let Some(Ok(message)) = socket.recv().await {
            if matches!(message, axum::extract::ws::Message::Close(_)) {
                break;
            }
            if socket.send(message).await.is_err() {
                break;
            }
        }
    })
}

#[allow(clippy::too_many_arguments)]
async fn http_load(
    client: &reqwest::Client,
    base: &str,
    label: &str,
    body: Bytes,
    concurrency: usize,
    count: usize,
    mode: &str,
    pid: Option<u32>,
) -> anyhow::Result<Value> {
    let large_body = (mode == "mixed").then(|| payload(8 * 1024 * 1024));
    let peak = Arc::new(AtomicU64::new(pid.map(rss_bytes).unwrap_or_default()));
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let monitor = tokio::spawn({
        let peak = peak.clone();
        async move {
            let mut stopped = stopped;
            loop {
                if let Some(pid) = pid {
                    peak.fetch_max(rss_bytes(pid), Ordering::Relaxed);
                }
                tokio::select! { _ = &mut stopped => break, _ = tokio::time::sleep(Duration::from_millis(10)) => {} }
            }
        }
    });
    let started = Instant::now();
    let results = stream::iter(0..count)
        .map(|index| {
            let body = if index % 100 == 0 { large_body.as_ref().unwrap_or(&body).clone() } else { body.clone() };
            let scheduled = started + Duration::from_millis(index as u64 * 5);
            async move {
                if matches!(mode, "steady" | "mixed") {
                    tokio::time::sleep_until(tokio::time::Instant::from_std(scheduled)).await;
                }
                let started = Instant::now();
                let mut request = client
                    .post(format!("{base}/v1/chat/completions"))
                    .header("content-type", "application/json")
                    .body(body);
                if mode == "stream" {
                    request = request.header("x-test-stream", "1");
                }
                if mode == "large_response" {
                    request = request.header("x-test-large-response", "1");
                }
                let response = request.send().await?.error_for_status()?;
                let mut chunks = response.bytes_stream();
                let mut first = None;
                let mut bytes = 0;
                while let Some(chunk) = chunks.next().await {
                    let chunk = chunk?;
                    if !chunk.is_empty() {
                        first.get_or_insert(started.elapsed().as_secs_f64() * 1000.0);
                    }
                    bytes += chunk.len();
                }
                let expected = match mode {
                    "stream" => 11 * b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n".len()
                        + b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":10}}\n\ndata: [DONE]\n\n".len(),
                    "large_response" => 8 * 1024 * 1024 + 58,
                    _ => br#"{"model":"test","choices":[{"message":{"role":"assistant","content":"hello"}}],"usage":{"prompt_tokens":10,"completion_tokens":1}}"#.len(),
                };
                anyhow::ensure!(bytes == expected, "response length changed: {bytes}, expected {expected}");
                Ok::<_, anyhow::Error>((
                    started.elapsed().as_secs_f64() * 1000.0,
                    first.unwrap_or_default(),
                ))
            }
        })
        .buffer_unordered(concurrency)
        .collect::<Vec<_>>()
        .await;
    let wall = started.elapsed().as_secs_f64();
    let _ = stop.send(());
    monitor.await?;
    let failures: Vec<String> = results
        .iter()
        .filter_map(|r| r.as_ref().err().map(ToString::to_string))
        .collect();
    let latency: Vec<_> = results
        .iter()
        .filter_map(|r| r.as_ref().ok().map(|r| r.0))
        .collect();
    let first: Vec<_> = results
        .iter()
        .filter_map(|r| r.as_ref().ok().map(|r| r.1))
        .collect();
    let result = json!({"case":label,"concurrency":concurrency,"requests":count,"request_bytes":body.len(),"elapsed_secs":wall,"rps":count as f64/wall,"latency_p50_ms":percentile(&latency,0.5),"latency_p95_ms":percentile(&latency,0.95),"latency_p99_ms":percentile(&latency,0.99),"first_byte_p95_ms":percentile(&first,0.95),"peak_rss_bytes":peak.load(Ordering::Relaxed),"failures":failures});
    eprintln!("PERF {result}");
    anyhow::ensure!(failures.is_empty(), "load case failed: {result}");
    Ok(result)
}

fn payload(size: usize) -> Bytes {
    let text: String = rand::rngs::StdRng::seed_from_u64(42)
        .sample_iter(&Alphanumeric)
        .take(size)
        .map(char::from)
        .collect();
    Bytes::from(
        serde_json::to_vec(&json!({"model":"test","messages":[{"role":"user","content":text}]}))
            .unwrap(),
    )
}

async fn websocket_load(base: &str) -> anyhow::Result<Value> {
    let started = Instant::now();
    let results = stream::iter(0..16)
        .map(|_| async move {
            let (mut socket, _) = tokio_tungstenite::connect_async(format!(
                "{}/v1/realtime",
                base.replacen("http", "ws", 1)
            ))
            .await?;
            let mut timings = Vec::new();
            for _ in 0..20 {
                let start = Instant::now();
                socket
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        "hello".into(),
                    ))
                    .await?;
                let reply = socket.next().await.context("websocket ended")??;
                anyhow::ensure!(reply.to_text()? == "hello", "wrong websocket echo");
                timings.push(start.elapsed().as_secs_f64() * 1000.0);
            }
            socket.close(None).await?;
            Ok::<_, anyhow::Error>(timings)
        })
        .buffer_unordered(16)
        .collect::<Vec<_>>()
        .await;
    let mut timings = Vec::new();
    for result in results {
        timings.extend(result?);
    }
    let result = json!({"case":"websocket_16","round_trips":timings.len(),"elapsed_secs":started.elapsed().as_secs_f64(),"rtt_p95_ms":percentile(&timings,0.95)});
    eprintln!("PERF {result}");
    Ok(result)
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "opt-in performance benchmark: build release binary; needs local PostgreSQL and mock servers"]
async fn performance_end_to_end(pool: PgPool) -> anyhow::Result<()> {
    let upstream = Router::new()
        .route("/v1/chat/completions", post(mock_response))
        .route("/v1/realtime", get(mock_websocket))
        .layer(axum::extract::DefaultBodyLimit::disable())
        .with_state(payload(8 * 1024 * 1024));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let upstream_url = format!("http://{}", listener.local_addr()?);
    let listener = listener.tap_io(|socket| {
        socket.set_nodelay(true).unwrap();
    });
    let upstream_handle = tokio::spawn(async move { axum::serve(listener, upstream).await });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(128)
        .build()?;
    let mut report = Vec::new();
    for backend in [
        ArchiveStorageBackend::Postgres,
        ArchiveStorageBackend::Filesystem,
    ] {
        let server = Server::start(&pool, &upstream_url, backend, 0, false).await?;
        let cookie = server.login(&client).await?;
        http_load(
            &client,
            &server.base,
            "warmup",
            payload(128),
            8,
            64,
            "json",
            Some(server.child.id()),
        )
        .await?;
        server.settle(&client).await?;
        for (mode, bytes, concurrency, count) in [
            ("json", 1024, 1, 100),
            ("json", 1024, 16, 512),
            ("json", 1024, 64, 1024),
            ("steady", 1024, 16, 1000),
            ("stream", 1024, 16, 128),
            ("stream", 1024, 64, 256),
            ("json", 8 * 1024 * 1024, 4, 16),
            ("json", 8 * 1024 * 1024, 16, 32),
            ("large_response", 1024, 4, 16),
        ] {
            let body = payload(bytes);
            report.push(
                http_load(
                    &client,
                    &upstream_url,
                    &format!("direct_{mode}_{bytes}_{concurrency}"),
                    body.clone(),
                    concurrency,
                    count,
                    mode,
                    None,
                )
                .await?,
            );
            let before = server.metrics(&client).await?;
            let mut result = http_load(
                &client,
                &server.base,
                &format!("{}_{mode}_{bytes}_{concurrency}", backend.as_str()),
                body,
                concurrency,
                count,
                mode,
                Some(server.child.id()),
            )
            .await?;
            let at_completion = server.metrics(&client).await?;
            result["pending_at_response_completion"] = json!(
                metric(&at_completion, "enqueued_total")
                    - metric(&at_completion, "persisted_total")
            );
            result["drain_secs"] = json!(server.settle(&client).await?);
            let after = server.metrics(&client).await?;
            result["trace_metrics_delta"] = json!(
                after
                    .iter()
                    .map(|(k, v)| (k, v - before.get(k).copied().unwrap_or_default()))
                    .collect::<BTreeMap<_, _>>()
            );
            anyhow::ensure!(
                metric(&after, "persist_failures_total") == 0.0
                    && metric(&after, "build_failures_total") == 0.0,
                "trace processing failed: {after:?}; inspect {}",
                server.root.display()
            );
            if mode == "steady" {
                anyhow::ensure!(
                    metric(&after, "persisted_total") - metric(&before, "persisted_total")
                        == count as f64,
                    "steady load lost traces"
                );
            }
            report.push(result);
        }
        report.push(websocket_load(&server.base).await?);
        server.settle(&client).await?;
        for endpoint in [
            "/api/requests?limit=100",
            "/api/sessions?limit=100",
            "/api/stats",
            "/api/usage/summary",
            "/api/retention/status",
        ] {
            let mut timings = Vec::new();
            for _ in 0..12 {
                let start = Instant::now();
                client
                    .get(format!("{}{endpoint}", server.base))
                    .header("cookie", &cookie)
                    .send()
                    .await?
                    .error_for_status()?
                    .bytes()
                    .await?;
                timings.push(start.elapsed().as_secs_f64() * 1000.0);
            }
            let result = json!({"case":format!("{}_{endpoint}",backend.as_str()),"latency_p95_ms":percentile(&timings,0.95)});
            eprintln!("PERF {result}");
            report.push(result);
        }
        if backend == ArchiveStorageBackend::Postgres
            && std::env::var_os("LLMTRACE_PERF_BROWSER").is_some()
        {
            let id: Uuid = sqlx::query_scalar("SELECT id FROM request_traces WHERE request_body_bytes > 8000000 ORDER BY started_at DESC LIMIT 1").fetch_one(&pool).await?;
            let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../scripts/browser-performance.mjs");
            let base = server.base.clone();
            let output = tokio::task::spawn_blocking(move || {
                Command::new("node")
                    .arg(script)
                    .arg(base)
                    .arg(id.to_string())
                    .output()
            })
            .await??;
            anyhow::ensure!(
                output.status.success(),
                "browser checks failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let result: Value = serde_json::from_slice(&output.stdout)?;
            eprintln!("BROWSER {result}");
            report.push(json!({"case":"browser", "result":result}));
        }
        // Simulate stalled storage while requests continue. The transaction is
        // rolled back before checking persistence; no live database is touched.
        let mut blocker = pool.begin().await?;
        sqlx::query("LOCK TABLE request_traces IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *blocker)
            .await?;
        let mut result = http_load(
            &client,
            &server.base,
            &format!("{}_blocked_database_8mib", backend.as_str()),
            payload(8 * 1024 * 1024),
            8,
            64,
            "json",
            Some(server.child.id()),
        )
        .await?;
        result["blocked_metrics"] = json!(server.metrics(&client).await?);
        let blocked = server.metrics(&client).await?;
        if metric(&blocked, "memory_limit_bytes") > 0.0 {
            anyhow::ensure!(
                metric(&blocked, "memory_used_bytes") <= metric(&blocked, "memory_limit_bytes"),
                "trace memory budget exceeded"
            );
            if blocked
                .get("llmtrace_journal_enabled")
                .copied()
                .unwrap_or_default()
                == 0.0
            {
                anyhow::ensure!(
                    metric(&blocked, "dropped_memory_total") > 0.0,
                    "blocked storage did not exercise memory shedding"
                );
            } else {
                anyhow::ensure!(
                    blocked
                        .get("llmtrace_journal_pending_records")
                        .copied()
                        .unwrap_or_default()
                        > 0.0,
                    "stalled storage did not leave journal records pending"
                );
            }
        }
        blocker.rollback().await?;
        result["drain_secs"] = json!(server.settle(&client).await?);
        let drained = server.metrics(&client).await?;
        anyhow::ensure!(
            metric(&drained, "enqueued_total") == metric(&drained, "persisted_total")
                && metric(&drained, "memory_used_bytes") == 0.0,
            "accepted traces did not persist and release memory after the database recovered: {drained:?}"
        );
        result["drained_metrics"] = json!(drained);
        report.push(result);
        let root = server.root.clone();
        drop(server);
        // Clear only the isolated benchmark DB, including indexed filesystem
        // archives from this phase, before moving to another backend.
        sqlx::query("TRUNCATE request_traces, trace_sessions, trace_rollups_minute, payload_archive_segments, archive_file_deletions CASCADE").execute(&pool).await?;
        fs::remove_dir_all(root)?;
    }
    let server = Server::start(
        &pool,
        &upstream_url,
        ArchiveStorageBackend::Postgres,
        0,
        true,
    )
    .await?;
    let mut plugin_result = http_load(
        &client,
        &server.base,
        "slow_plugin_100ms",
        payload(1024),
        16,
        32,
        "json",
        Some(server.child.id()),
    )
    .await?;
    plugin_result["drain_secs"] = json!(server.settle(&client).await?);
    let metrics = server.metrics(&client).await?;
    anyhow::ensure!(
        metric(&metrics, "persisted_total") == 32.0,
        "plugin timeout lost traces"
    );
    plugin_result["metrics"] = json!(metrics);
    report.push(plugin_result);
    let root = server.root.clone();
    drop(server);
    fs::remove_dir_all(root)?;
    upstream_handle.abort();
    let path = std::env::var_os("LLMTRACE_PERF_REPORT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("llmtrace-performance.json"));
    fs::write(
        &path,
        serde_json::to_vec_pretty(
            &json!({"logical_cpus":std::thread::available_parallelism()?.get(),"results":report}),
        )?,
    )?;
    eprintln!("Performance report: {}", path.display());
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "opt-in 50,000-request storage/rotation benchmark; requires local PostgreSQL"]
async fn performance_storage_scaling(pool: PgPool) -> anyhow::Result<()> {
    let archive = ArchiveConfig {
        storage_backend: ArchiveStorageBackend::Postgres,
        ..Default::default()
    };
    let body = payload(1024);
    let plugins = crate::plugins::PluginManager::load(&[])?;
    let mut event = crate::trace::TraceEvent::base(Uuid::new_v4(), Utc::now());
    event.original_uri = "/v1/chat/completions".into();
    event.request_body = body.to_vec();
    event.request_body_bytes = body.len() as i64;
    event.status = Some(200);
    event.duration_ms = Some(100);
    let (trace, messages, uid, name) = crate::trace::build_trace(event, &plugins)?;
    let id = trace.id;
    insert_trace(&pool, &archive, trace, messages, uid, name).await?;
    let started = Instant::now();
    sqlx::query(r#"INSERT INTO request_traces (id, started_at, method, original_uri, upstream_url, session_id, session_key, status, duration_ms, request_body_bytes, response_body_bytes)
        SELECT md5('perf-trace-' || g)::uuid, now() - interval '20 hours' + g * interval '1 second', method, original_uri, upstream_url, session_id, session_key, status, duration_ms, request_body_bytes, response_body_bytes
        FROM request_traces CROSS JOIN generate_series(1,50000) g WHERE id = $1"#).bind(id).execute(&pool).await?;
    sqlx::query(r#"INSERT INTO payload_archive_segments (id, session_id, segment_index, uncompressed_bytes, compressed_bytes, record_count, compression_level, storage_backend, storage_key, checksum_sha256, sealed)
        SELECT md5('perf-segment-' || g)::uuid, s.session_id, g, s.uncompressed_bytes, s.compressed_bytes, s.record_count, s.compression_level, s.storage_backend, md5('perf-segment-' || g), s.checksum_sha256, true
        FROM payload_archive_segments s CROSS JOIN generate_series(1,50000) g WHERE s.id = (SELECT segment_id FROM payload_archive_records WHERE trace_id = $1 LIMIT 1)"#).bind(id).execute(&pool).await?;
    sqlx::query(r#"INSERT INTO payload_archive_segment_blobs (segment_id, compressed_payload)
        SELECT md5('perf-segment-' || g)::uuid, b.compressed_payload FROM payload_archive_segment_blobs b CROSS JOIN generate_series(1,50000) g
        WHERE b.segment_id = (SELECT segment_id FROM payload_archive_records WHERE trace_id = $1 LIMIT 1)"#).bind(id).execute(&pool).await?;
    sqlx::query(r#"INSERT INTO payload_archive_records (id, trace_id, session_id, segment_id, record_index, direction, content_type, uncompressed_offset, uncompressed_len, body_sha256)
        SELECT md5('perf-record-' || g || r.direction)::uuid, md5('perf-trace-' || g)::uuid, r.session_id, md5('perf-segment-' || g)::uuid, r.record_index, r.direction, r.content_type, r.uncompressed_offset, r.uncompressed_len, r.body_sha256
        FROM payload_archive_records r CROSS JOIN generate_series(1,50000) g WHERE r.trace_id = $1"#).bind(id).execute(&pool).await?;
    sqlx::query(r#"INSERT INTO session_messages (request_id,session_id,role,content,created_at)
        SELECT id,session_id,'user','synthetic preview',started_at FROM request_traces WHERE id <> $1"#).bind(id).execute(&pool).await?;
    sqlx::raw_sql(r#"TRUNCATE trace_rollups_minute;
        INSERT INTO trace_rollups_minute (bucket,last_seen,total,errors,captured_bytes,duration_count,duration_sum_ms,ttft_count,ttft_sum_ms)
        SELECT date_trunc('minute',started_at),max(started_at),count(*),count(*) FILTER(WHERE status>=400 OR error IS NOT NULL),sum(request_body_bytes+response_body_bytes),count(duration_ms),COALESCE(sum(duration_ms),0),count(ttft_ms),COALESCE(sum(ttft_ms),0) FROM request_traces GROUP BY 1;
        ANALYZE request_traces; ANALYZE payload_archive_segments; ANALYZE payload_archive_records; ANALYZE trace_rollups_minute; ANALYZE session_messages;"#).execute(&pool).await?;
    let seed_secs = started.elapsed().as_secs_f64();
    let session: Uuid = sqlx::query_scalar("SELECT session_id FROM request_traces WHERE id=$1")
        .bind(id)
        .fetch_one(&pool)
        .await?;
    let mut timings = BTreeMap::new();
    for _ in 0..5 {
        let start = Instant::now();
        list_requests(&pool, RequestListFilters::default()).await?;
        timings
            .entry("requests_ms")
            .or_insert_with(Vec::new)
            .push(start.elapsed().as_secs_f64() * 1000.0);
        let start = Instant::now();
        get_session(&pool, session, None, None).await?;
        timings
            .entry("session_ms")
            .or_insert_with(Vec::new)
            .push(start.elapsed().as_secs_f64() * 1000.0);
        let start = Instant::now();
        stats(&pool).await?;
        timings
            .entry("stats_ms")
            .or_insert_with(Vec::new)
            .push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let mut tx = pool.begin().await?;
    let before = rotation::archive_footprint(&mut tx).await?.retained_bytes;
    tx.commit().await?;
    let config = StorageConfig {
        rotate_size_bytes: (before * 9 / 10) as u64,
        ..Default::default()
    };
    let started = Instant::now();
    let writes = async {
        let mut durations = Vec::new();
        for _ in 0..100 {
            let mut event = crate::trace::TraceEvent::base(Uuid::new_v4(), Utc::now());
            event.request_body = body.to_vec();
            event.request_body_bytes = body.len() as i64;
            event.status = Some(200);
            let (trace, messages, uid, name) = crate::trace::build_trace(event, &plugins)?;
            let start = Instant::now();
            insert_trace(&pool, &archive, trace, messages, uid, name).await?;
            durations.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        Ok::<_, anyhow::Error>(durations)
    };
    let (deleted, writes) =
        tokio::try_join!(rotation::maintain_archive(&pool, &config, &archive), writes)?;
    let rotation_secs = started.elapsed().as_secs_f64();
    let mut tx = pool.begin().await?;
    let after = rotation::archive_footprint(&mut tx).await?.retained_bytes;
    tx.commit().await?;
    let rollup_total: i64 =
        sqlx::query_scalar("SELECT SUM(total)::bigint FROM trace_rollups_minute")
            .fetch_one(&pool)
            .await?;
    let trace_total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_traces")
        .fetch_one(&pool)
        .await?;
    assert_eq!(rollup_total, trace_total);
    anyhow::ensure!(
        after as u64 <= config.rotate_size_bytes,
        "rotation exceeded its size limit"
    );
    let result = json!({"case":"storage_50000", "seed_secs":seed_secs,"query_p95_ms":timings.into_iter().map(|(k,v)|(k,percentile(&v,0.95))).collect::<BTreeMap<_,_>>(),"rotation_secs":rotation_secs,"concurrent_write_p95_ms":percentile(&writes,0.95),"deleted_requests":deleted,"before_bytes":before,"after_bytes":after,"limit_bytes":config.rotate_size_bytes,"within_limit":after as u64 <= config.rotate_size_bytes});
    eprintln!("PERF {result}");
    if let Some(path) = std::env::var_os("LLMTRACE_PERF_STORAGE_REPORT") {
        fs::write(path, serde_json::to_vec_pretty(&result)?)?;
    }
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "opt-in two-minute mixed-load/rotation test; build release binary and enable local PostgreSQL"]
async fn performance_sustained_rotation(pool: PgPool) -> anyhow::Result<()> {
    let upstream = Router::new()
        .route("/v1/chat/completions", post(mock_response))
        .layer(axum::extract::DefaultBodyLimit::disable())
        .with_state(Bytes::new());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let upstream_url = format!("http://{}", listener.local_addr()?);
    let listener = listener.tap_io(|socket| socket.set_nodelay(true).unwrap());
    let upstream_handle = tokio::spawn(async move { axum::serve(listener, upstream).await });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let mut results = Vec::new();
    for backend in [
        ArchiveStorageBackend::Postgres,
        ArchiveStorageBackend::Filesystem,
    ] {
        let server = Server::start(&pool, &upstream_url, backend, 32 * 1024 * 1024, false).await?;
        let pid = server.child.id();
        let (stop, mut stopped) = tokio::sync::oneshot::channel::<()>();
        let timeline = tokio::spawn(async move {
            let start = Instant::now();
            let mut points = Vec::new();
            loop {
                let point = json!({"elapsed_secs":start.elapsed().as_secs_f64(),"rss_bytes":rss_bytes(pid)});
                eprintln!("SOAK {} {point}", backend.as_str());
                points.push(point);
                tokio::select! { _ = &mut stopped => break, _ = tokio::time::sleep(Duration::from_secs(15)) => {} }
            }
            points
        });
        let mut result = http_load(
            &client,
            &server.base,
            &format!("{}_mixed_200rps_rotation", backend.as_str()),
            payload(1024),
            64,
            12000,
            "mixed",
            Some(pid),
        )
        .await?;
        result["drain_secs"] = json!(server.settle(&client).await?);
        let _ = stop.send(());
        result["rss_timeline"] = json!(timeline.await?);
        result["large_request_every"] = json!(100);
        let metrics = server.metrics(&client).await?;
        anyhow::ensure!(
            metric(&metrics, "persisted_total") == 12000.0,
            "sustained load lost traces: {metrics:?}"
        );
        anyhow::ensure!(
            metric(&metrics, "memory_used_bytes") == 0.0,
            "memory reservations were not released"
        );
        anyhow::ensure!(
            metrics
                .get("llmtrace_retention_failures_total")
                .copied()
                .unwrap_or_default()
                == 0.0,
            "rotation failed during sustained load"
        );
        result["metrics"] = json!(metrics);
        let config = StorageConfig {
            rotate_size_bytes: 32 * 1024 * 1024,
            rotate_check_interval_secs: 1,
            ..Default::default()
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let status = retention_status(&pool, &config).await?;
            if status["rotation"]["over_limit"] == false
                && status["rotation"]["pending_delete_files"] == 0
            {
                result["rotation"] = status["rotation"].clone();
                break;
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "rotation did not catch up after load stopped"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let counts: (i64,i64) = sqlx::query_as("SELECT (SELECT COUNT(*) FROM request_traces), (SELECT COALESCE(SUM(total),0)::bigint FROM trace_rollups_minute)").fetch_one(&pool).await?;
        assert_eq!(counts.0, counts.1);
        anyhow::ensure!(counts.0 < 12000, "size rotation did not evict old requests");
        result["retained_requests"] = json!(counts.0);
        results.push(result);
        let root = server.root.clone();
        drop(server);
        sqlx::query("TRUNCATE request_traces, trace_sessions, trace_rollups_minute, payload_archive_segments, archive_file_deletions CASCADE").execute(&pool).await?;
        fs::remove_dir_all(root)?;
    }
    upstream_handle.abort();
    let path = std::env::var_os("LLMTRACE_PERF_SOAK_REPORT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("llmtrace-soak.json"));
    fs::write(path, serde_json::to_vec_pretty(&results)?)?;
    Ok(())
}
