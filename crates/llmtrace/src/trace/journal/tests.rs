use super::*;
use chrono::Utc;
use std::io::{Seek, SeekFrom};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("llmtrace-journal-test-{}", Uuid::new_v4())))
    }
    fn config(&self) -> JournalConfig {
        JournalConfig {
            directory: self.0.clone(),
            ..Default::default()
        }
    }
    fn open(&self) -> Arc<Journal> {
        Journal::open(&self.config(), "postgres://test:secret@localhost/test").unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn event() -> TraceEvent {
    let mut event = TraceEvent::base(Uuid::new_v4(), Utc::now());
    event.request_body = vec![0, 255, 1, 128];
    event.response_body = "多字节 payload".as_bytes().to_vec();
    event.plugin_request_headers = serde_json::json!({"authorization":"Bearer sensitive-test-key"});
    event
}

#[test]
fn journal_round_trip_is_immutable_private_and_recoverable() {
    let fixture = Fixture::new();
    let journal = fixture.open();
    let expected = event();
    journal.append(&expected).unwrap();
    let namespace = journal.id;
    let bytes = fs::read(journal.path(expected.id)).unwrap();
    assert!(journal.append(&expected).is_err());
    assert_eq!(fs::read(journal.path(expected.id)).unwrap(), bytes);
    assert!(Journal::open(&fixture.config(), "postgres://test:secret@localhost/test").is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(journal.path(expected.id))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&fixture.0).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    drop(journal);
    let journal = fixture.open();
    assert_eq!(journal.id, namespace);
    let actual = journal.read(expected.id).unwrap();
    assert_eq!(actual.request_body, expected.request_body);
    assert_eq!(actual.response_body, expected.response_body);
    assert_eq!(
        serde_json::to_value(&actual).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
    let budget = MemoryBudget::new(1024 * 1024);
    let claim = journal.claim(&budget).unwrap();
    assert!(claim.recovered);
    assert!(budget.used() > 0);
    journal.acknowledge(claim.id).unwrap();
    drop(claim);
    assert_eq!(budget.used(), 0);
    assert_eq!(journal.snapshot(budget.limit).pending_bytes, 0);
    drop(journal);
    assert_eq!(fixture.open().snapshot(budget.limit).pending_records, 0);
}

#[test]
fn journal_capacity_preserves_existing_records_and_counts_quarantine() {
    let fixture = Fixture::new();
    let mut config = fixture.config();
    config.max_records = 1;
    let journal = Journal::open(&config, "postgres://localhost/test").unwrap();
    let first = event();
    journal.append(&first).unwrap();
    assert!(journal.append(&event()).unwrap_err().is::<Full>());
    journal.quarantine(first.id).unwrap();
    assert!(journal.append(&event()).unwrap_err().is::<Full>());
    assert_eq!(journal.snapshot(1024 * 1024).quarantined_records, 1);
    drop(journal);
    config.max_records = 10;
    config.max_bytes = HEADER_BYTES;
    let journal = Journal::open(&config, "postgres://localhost/test").unwrap();
    assert!(journal.append(&event()).unwrap_err().is::<Full>());
}

#[test]
fn journal_checksums_cover_body_boundaries_and_body_bytes() {
    let fixture = Fixture::new();
    let journal = fixture.open();
    let event = event();
    journal.append(&event).unwrap();
    let path = journal.path(event.id);
    let original = fs::read(&path).unwrap();
    let mut corrupt = original.clone();
    let request = u64::from_le_bytes(corrupt[16..24].try_into().unwrap());
    let response = u64::from_le_bytes(corrupt[24..32].try_into().unwrap());
    corrupt[16..24].copy_from_slice(&(request + 1).to_le_bytes());
    corrupt[24..32].copy_from_slice(&(response - 1).to_le_bytes());
    fs::write(&path, &corrupt).unwrap();
    assert!(
        journal
            .read(event.id)
            .unwrap_err()
            .to_string()
            .contains("checksum")
    );
    let mut corrupt = original;
    *corrupt.last_mut().unwrap() ^= 1;
    fs::write(&path, &corrupt).unwrap();
    assert!(journal.read(event.id).is_err());
}

#[test]
fn journal_recovers_healthy_records_and_quarantines_interrupted_writes() {
    let fixture = Fixture::new();
    let journal = fixture.open();
    let good = event();
    journal.append(&good).unwrap();
    fs::write(
        fixture.0.join(format!("{}.partial", Uuid::new_v4())),
        b"partial",
    )
    .unwrap();
    fs::write(
        fixture.0.join(format!("{}.trace", Uuid::new_v4())),
        b"bad header",
    )
    .unwrap();
    drop(journal);
    let journal = fixture.open();
    assert_eq!(journal.snapshot(1024 * 1024).pending_records, 1);
    assert_eq!(journal.snapshot(1024 * 1024).quarantined_records, 2);
    assert_eq!(journal.read(good.id).unwrap().id, good.id);
    drop(journal);
    assert_eq!(fixture.open().snapshot(1024 * 1024).quarantined_records, 2);
}

#[test]
fn journal_preserves_records_that_exceed_a_reduced_memory_budget() {
    let fixture = Fixture::new();
    let journal = fixture.open();
    let event = event();
    journal.append(&event).unwrap();
    let budget = MemoryBudget::new(16);
    assert!(journal.claim(&budget).is_none());
    assert_eq!(journal.snapshot(budget.limit).blocked_records, 1);
    assert_eq!(budget.used(), 0);
    let budget = MemoryBudget::new(1024 * 1024);
    let claim = journal.claim(&budget).unwrap();
    assert!(journal.claim(&budget).is_none());
    drop(claim); // Simulate a cancelled processing task.
    assert_eq!(budget.used(), 0);
    journal
        .catalog
        .lock()
        .unwrap()
        .entries
        .get_mut(&event.id)
        .unwrap()
        .due = Instant::now();
    assert_eq!(journal.claim(&budget).unwrap().id, event.id);
}

#[test]
fn journal_rejects_wrong_database_missing_manifest_and_oversized_headers() {
    let fixture = Fixture::new();
    let journal = fixture.open();
    let event = event();
    journal.append(&event).unwrap();
    let path = journal.path(event.id);
    drop(journal);
    for target in [
        "postgres://localhost/other",
        "postgres://localhost/test?dbname=other",
        "postgres://localhost/test?host=other",
        "postgres://localhost/test?port=5433",
    ] {
        assert!(Journal::open(&fixture.config(), target).is_err());
    }
    // Credential rotation keeps the journal bound to the same explicit database.
    drop(
        Journal::open(
            &fixture.config(),
            "postgres://rotated:new@localhost/test?password=changed",
        )
        .unwrap(),
    );
    let mut file = OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(8)).unwrap();
    file.write_all(&u64::MAX.to_le_bytes()).unwrap();
    let journal = fixture.open();
    assert_eq!(journal.snapshot(1024 * 1024).quarantined_records, 1);
    drop(journal);
    fs::remove_file(fixture.0.join("manifest.json")).unwrap();
    assert!(Journal::open(&fixture.config(), "postgres://localhost/test").is_err());
}

// Invoked only by the subprocess tests below. No production failpoints exist.
#[tokio::test]
#[ignore = "subprocess helper; does nothing without the private test environment"]
async fn journal_process_helper() -> anyhow::Result<()> {
    let Some(root) = std::env::var_os("LLMTRACE_TEST_JOURNAL_ROOT") else {
        return Ok(());
    };
    let root = PathBuf::from(root);
    let url = std::env::var("DATABASE_URL")?;
    let config = JournalConfig {
        directory: root.join("journal"),
        ..Default::default()
    };
    let journal = Journal::open(&config, &url)?;
    let mut capture = event();
    capture.id = Uuid::parse_str(&std::env::var("LLMTRACE_TEST_TRACE_ID")?)?;
    capture.original_uri = "/v1/chat/completions".into();
    capture.request_body = serde_json::to_vec(
        &serde_json::json!({"messages":[{"role":"user","content":"x".repeat(1024*1024)}]}),
    )?;
    capture.request_body_bytes = capture.request_body.len() as i64;
    capture.status = Some(200);
    capture.response_body =
        br#"{"choices":[{"message":{"role":"assistant","content":"hello"}}]}"#.to_vec();
    capture.response_body_bytes = capture.response_body.len() as i64;
    journal.append(&capture)?;
    if std::env::var("LLMTRACE_TEST_CRASH_STAGE")?.as_str() == "committed" {
        let pool = sqlx::PgPool::connect(&url).await?;
        let plugins = crate::plugins::PluginManager::load(&[])?;
        let built = crate::trace::build_trace(capture, &plugins)?;
        let archive = crate::config::ArchiveConfig {
            storage_backend: crate::config::ArchiveStorageBackend::Postgres,
            ..Default::default()
        };
        crate::storage::insert_journaled_trace(
            &pool, &archive, built.0, built.1, built.2, built.3, journal.id,
        )
        .await?;
    }
    fs::write(root.join("ready"), b"durable checkpoint reached")?;
    // Parent kills this actual process without unwinding or journal cleanup.
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn database_url(pool: &sqlx::PgPool) -> anyhow::Result<String> {
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(pool)
        .await?;
    let mut url = url::Url::parse(&std::env::var("DATABASE_URL")?)?;
    url.set_path(&database);
    Ok(url.to_string())
}

async fn crash_child(fixture: &Fixture, url: &str, id: Uuid, stage: &str) -> anyhow::Result<()> {
    fs::create_dir_all(&fixture.0)?;
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "trace::journal::tests::journal_process_helper",
            "--ignored",
            "--nocapture",
        ])
        .env("LLMTRACE_TEST_JOURNAL_ROOT", &fixture.0)
        .env("DATABASE_URL", url)
        .env("LLMTRACE_TEST_TRACE_ID", id.to_string())
        .env("LLMTRACE_TEST_CRASH_STAGE", stage)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(20);
    while !fixture.0.join("ready").exists() {
        if child.try_wait()?.is_some() {
            anyhow::bail!("crash helper exited before durable checkpoint");
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("crash helper timed out");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    child.kill()?;
    assert!(!child.wait()?.success());
    Ok(())
}

fn storage_config(fixture: &Fixture, url: &str) -> crate::config::StorageConfig {
    crate::config::StorageConfig {
        postgres_url: url.into(),
        journal: JournalConfig {
            directory: fixture.0.join("journal"),
            retry_interval_secs: 1,
            ..Default::default()
        },
        ..Default::default()
    }
}

async fn await_replay(recorder: &crate::trace::TraceRecorder) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = recorder.queue_metrics();
        if status.journal.pending_records == 0 && status.memory_used_bytes == 0 && status.depth == 0
        {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "journal replay did not drain: {status:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL; kills isolated helper processes"]
async fn journal_replays_after_kill_and_does_not_resurrect_rotated_commits(
    pool: sqlx::PgPool,
) -> anyhow::Result<()> {
    let url = database_url(&pool).await?;
    for stage in ["journaled", "committed"] {
        let fixture = Fixture::new();
        let id = Uuid::new_v4();
        crash_child(&fixture, &url, id, stage).await?;
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM request_traces WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(before, i64::from(stage == "committed"));
        if stage == "committed" {
            // Retention can delete a trace while its journal acknowledgement is pending.
            sqlx::query("DELETE FROM request_traces WHERE id=$1")
                .bind(id)
                .execute(&pool)
                .await?;
        }
        let plugins = Arc::new(crate::plugins::PluginManager::load(&[])?);
        let metrics = crate::metrics::RuntimeMetrics::default();
        let archive = crate::config::ArchiveConfig {
            storage_backend: crate::config::ArchiveStorageBackend::Postgres,
            ..Default::default()
        };
        let (recorder, pipeline) = crate::trace::TraceRecorder::spawn(
            pool.clone(),
            plugins,
            archive.clone(),
            Arc::new(Default::default()),
            &storage_config(&fixture, &url),
            metrics.clone(),
        )?;
        await_replay(&recorder).await?;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM request_traces WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(count, i64::from(stage == "journaled"));
        assert_eq!(
            metrics.snapshot(recorder.queue_metrics())["trace_journal"]["recovered"],
            1
        );
        if stage == "journaled" {
            let detail = crate::storage::get_request(&pool, id, &archive, true)
                .await?
                .unwrap();
            assert!(detail["request_body"].as_str().unwrap().len() > 1024 * 1024);
            let messages: i64 =
                sqlx::query_scalar("SELECT count(*) FROM session_messages WHERE request_id=$1")
                    .bind(id)
                    .fetch_one(&pool)
                    .await?;
            assert_eq!(messages, 2);
        }
        drop(recorder);
        pipeline.drain().await;
    }
    let counts:(i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM request_traces), (SELECT coalesce(sum(total),0)::bigint FROM trace_rollups_minute)").fetch_one(&pool).await?;
    assert_eq!(counts, (1, 1));
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL; simulates storage lock timeouts"]
async fn journal_keeps_capturing_during_database_failure_and_retries(
    pool: sqlx::PgPool,
) -> anyhow::Result<()> {
    let fixture = Fixture::new();
    let url = database_url(&pool).await?;
    let replay_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .after_connect(|connection, _| {
            Box::pin(async move {
                sqlx::query("SET lock_timeout='100ms'")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect(&url)
        .await?;
    let mut blocker = pool.begin().await?;
    sqlx::query("LOCK TABLE request_traces IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await?;
    let metrics = crate::metrics::RuntimeMetrics::default();
    let plugins = Arc::new(crate::plugins::PluginManager::load(&[])?);
    let archive = crate::config::ArchiveConfig {
        storage_backend: crate::config::ArchiveStorageBackend::Postgres,
        ..Default::default()
    };
    let (recorder, pipeline) = crate::trace::TraceRecorder::spawn(
        replay_pool.clone(),
        plugins,
        archive,
        Arc::new(Default::default()),
        &storage_config(&fixture, &url),
        metrics.clone(),
    )?;
    for _ in 0..8 {
        let mut capture = event();
        capture.request_body = vec![b'x'; 1024 * 1024];
        recorder.record(capture);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = metrics.snapshot(recorder.queue_metrics());
        if status["trace_journal"]["written"] == 8
            && status["trace_journal"]["retry"].as_u64().unwrap() > 0
        {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "journal did not continue through database outage: {status}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(recorder.queue_metrics().journal.pending_records, 8);
    blocker.rollback().await?;
    await_replay(&recorder).await?;
    assert_eq!(
        metrics.snapshot(recorder.queue_metrics())["trace_pipeline"]["persisted"],
        8
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM request_traces")
        .fetch_one(&pool)
        .await?;
    assert_eq!(count, 8);
    drop(recorder);
    pipeline.drain().await;
    replay_pool.close().await;
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL"]
async fn journal_receipt_makes_concurrent_replay_idempotent(
    pool: sqlx::PgPool,
) -> anyhow::Result<()> {
    let fixture = Fixture::new();
    let url = database_url(&pool).await?;
    let journal = Journal::open(&fixture.config(), &url)?;
    let mut capture = event();
    capture.original_uri = "/v1/chat/completions".into();
    capture.request_body = br#"{"messages":[{"role":"user","content":"hello"}]}"#.to_vec();
    capture.status = Some(200);
    capture.response_body =
        br#"{"choices":[{"message":{"role":"assistant","content":"hello"}}]}"#.to_vec();
    capture.response_body_bytes = capture.response_body.len() as i64;
    journal.append(&capture)?;
    let plugins = crate::plugins::PluginManager::load(&[])?;
    let archive = crate::config::ArchiveConfig {
        storage_backend: crate::config::ArchiveStorageBackend::Postgres,
        ..Default::default()
    };
    let insert = || async {
        let built = crate::trace::build_trace(journal.read(capture.id)?, &plugins)?;
        crate::storage::insert_journaled_trace(
            &pool, &archive, built.0, built.1, built.2, built.3, journal.id,
        )
        .await
    };
    tokio::try_join!(insert(), insert())?;
    let counts:(i64,i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM request_traces),(SELECT count(*) FROM session_messages),(SELECT sum(total)::bigint FROM trace_rollups_minute),(SELECT count(*) FROM payload_archive_records)").fetch_one(&pool).await?;
    assert_eq!(counts, (1, 2, 1, 2)); // One input preview plus the generic response preview.
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL"]
async fn journal_retry_reuses_files_after_database_transaction_rollback(
    pool: sqlx::PgPool,
) -> anyhow::Result<()> {
    let fixture = Fixture::new();
    let archive_fixture = Fixture::new();
    let url = database_url(&pool).await?;
    let journal = Journal::open(&fixture.config(), &url)?;
    let mut capture = event();
    capture.original_uri = "/v1/chat/completions".into();
    capture.request_body = br#"{"messages":[{"role":"user","content":"hello"}]}"#.to_vec();
    capture.request_body_bytes = capture.request_body.len() as i64;
    journal.append(&capture)?;
    let plugins = crate::plugins::PluginManager::load(&[])?;
    let archive = crate::config::ArchiveConfig {
        storage_backend: crate::config::ArchiveStorageBackend::Filesystem,
        filesystem_root: archive_fixture.0.clone(),
        ..Default::default()
    };
    sqlx::raw_sql("CREATE FUNCTION reject_test_rollup() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected persistence failure'; END; $$; CREATE TRIGGER reject_test_rollup BEFORE INSERT ON trace_rollups_minute FOR EACH ROW EXECUTE FUNCTION reject_test_rollup();").execute(&pool).await?;
    let insert = || async {
        let built = crate::trace::build_trace(journal.read(capture.id)?, &plugins)?;
        crate::storage::insert_journaled_trace(
            &pool, &archive, built.0, built.1, built.2, built.3, journal.id,
        )
        .await
    };
    for _ in 0..3 {
        assert!(insert().await.is_err());
    }
    let directory = archive_fixture
        .0
        .join("journal")
        .join(journal.id.to_string())
        .join(capture.id.to_string());
    assert_eq!(
        fs::read_dir(&directory)?.count(),
        1,
        "retries created orphan archive copies"
    );
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM trace_ingest_receipts")
        .fetch_one(&pool)
        .await?;
    assert_eq!(receipts, 0, "failed transaction left a success receipt");
    sqlx::raw_sql("DROP TRIGGER reject_test_rollup ON trace_rollups_minute; DROP FUNCTION reject_test_rollup();").execute(&pool).await?;
    insert().await?;
    assert_eq!(fs::read_dir(directory)?.count(), 1);
    let detail = crate::storage::get_request(&pool, capture.id, &archive, true)
        .await?
        .unwrap();
    assert_eq!(
        detail["request_body"].as_str().unwrap().as_bytes(),
        capture.request_body
    );
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and HTTP sockets"]
async fn journal_proxy_reports_complete_and_interrupted_payloads(
    pool: sqlx::PgPool,
) -> anyhow::Result<()> {
    use axum::{Router, body::Body, routing::get};
    use futures_util::StreamExt;
    let fixture = Fixture::new();
    let url = database_url(&pool).await?;
    let upstream = Router::new()
        .route(
            "/v1/plain",
            get(|| async {
                r#"{"choices":[{"message":{"role":"assistant","content":"complete"}}]}"#
            }),
        )
        .route(
            "/v1/partial",
            get(|| async {
                let chunks = futures_util::stream::once(async {
                    Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"data: partial\n\n"))
                })
                .chain(futures_util::stream::pending());
                (
                    [("content-type", "text/event-stream")],
                    Body::from_stream(chunks),
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let upstream_url = format!("http://{}", listener.local_addr()?);
    let handle = tokio::spawn(async move { axum::serve(listener, upstream).await });
    let mut config = crate::config::Config {
        storage: storage_config(&fixture, &url),
        ..Default::default()
    };
    config.proxy.default_upstream = upstream_url;
    config.archive.storage_backend = crate::config::ArchiveStorageBackend::Postgres;
    let plugins = Arc::new(crate::plugins::PluginManager::load(&[])?);
    let metrics = crate::metrics::RuntimeMetrics::default();
    let (recorder, pipeline) = crate::trace::TraceRecorder::spawn(
        pool.clone(),
        plugins.clone(),
        config.archive.clone(),
        Arc::new(Default::default()),
        &config.storage,
        metrics.clone(),
    )?;
    let state = crate::state::AppState::new(
        config.clone(),
        pool.clone(),
        plugins,
        recorder.clone(),
        metrics.clone(),
    )?;
    let app = crate::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let proxy_handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
    });
    let client = reqwest::Client::new();
    let mut ids = Vec::new();
    for (method, path) in [
        (reqwest::Method::GET, "/v1/plain"),
        (reqwest::Method::HEAD, "/v1/plain"),
        (reqwest::Method::GET, "/v1/partial"),
    ] {
        let response = client
            .request(method, format!("{base}{path}"))
            .send()
            .await?
            .error_for_status()?;
        ids.push(Uuid::parse_str(
            response.headers()["x-llmtrace-trace-id"].to_str()?,
        )?);
        if path.ends_with("plain") {
            response.bytes().await?;
        } else {
            let mut stream = response.bytes_stream();
            assert!(!stream.next().await.unwrap()?.is_empty());
            drop(stream);
        }
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while metrics.snapshot(recorder.queue_metrics())["trace_pipeline"]["enqueued"] != 3 {
        ensure!(Instant::now() < deadline, "proxy did not finalize captures");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    await_replay(&recorder).await?;
    for id in &ids[..2] {
        let complete = crate::storage::get_request(&pool, *id, &config.archive, false)
            .await?
            .unwrap();
        assert_eq!(complete["error"], serde_json::Value::Null);
        assert_eq!(complete["request_body_truncated"], false);
        assert_eq!(complete["response_body_truncated"], false);
    }
    let partial = crate::storage::get_request(&pool, ids[2], &config.archive, false)
        .await?
        .unwrap();
    assert_eq!(partial["response_body_truncated"], true);
    assert!(
        partial["tags"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("capture_interrupted"))
    );
    let complete:bool=sqlx::query_scalar("SELECT complete FROM payload_archive_records WHERE trace_id=$1 AND direction='response_body'").bind(ids[2]).fetch_one(&pool).await?;
    assert!(!complete);
    drop(client);
    let _ = stop.send(());
    proxy_handle.await??;
    drop(state);
    drop(recorder);
    pipeline.drain().await;
    handle.abort();
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL"]
async fn journal_quarantines_corruption_and_reports_full_without_stopping_replay(
    pool: sqlx::PgPool,
) -> anyhow::Result<()> {
    let fixture = Fixture::new();
    let url = database_url(&pool).await?;
    let mut config = storage_config(&fixture, &url);
    let journal = Journal::open(&config.journal, &url)?;
    let good = event();
    let bad = event();
    journal.append(&good)?;
    journal.append(&bad)?;
    let mut bytes = fs::read(journal.path(bad.id))?;
    *bytes.last_mut().unwrap() ^= 1;
    fs::write(journal.path(bad.id), bytes)?;
    drop(journal);
    // Lowering the cap preserves both existing records. A quarantined record
    // continues to consume capacity after the healthy record has drained.
    config.journal.max_records = 1;
    let metrics = crate::metrics::RuntimeMetrics::default();
    let (recorder, pipeline) = crate::trace::TraceRecorder::spawn(
        pool.clone(),
        Arc::new(crate::plugins::PluginManager::load(&[])?),
        crate::config::ArchiveConfig {
            storage_backend: crate::config::ArchiveStorageBackend::Postgres,
            ..Default::default()
        },
        Arc::new(Default::default()),
        &config,
        metrics.clone(),
    )?;
    await_replay(&recorder).await?;
    let rejected = event();
    let rejected_id = rejected.id;
    recorder.record(rejected);
    await_replay(&recorder).await?;
    let status = metrics.snapshot(recorder.queue_metrics());
    assert_eq!(status["trace_journal"]["quarantined_records"], 1);
    assert_eq!(status["trace_journal"]["read_failed"], 1);
    assert_eq!(status["trace_journal"]["dropped_full"], 1);
    assert_eq!(status["trace_journal"]["not_yet_durable"], 0);
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM request_traces")
        .fetch_all(&pool)
        .await?;
    assert_eq!(ids, vec![good.id]);
    assert!(!ids.contains(&rejected_id));
    assert!(fs::read_dir(&config.journal.directory)?.any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".quarantine")
    }));
    drop(recorder);
    pipeline.drain().await;
    Ok(())
}
