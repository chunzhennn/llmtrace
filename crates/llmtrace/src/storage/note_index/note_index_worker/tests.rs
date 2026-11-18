use super::*;

#[test]
fn index_options_reject_zero_unbounded_and_inconsistent_budgets() {
    let mut options = NoteIndexOptions::default();
    assert!(options.validate().is_ok());
    options.max_connections = 1;
    assert!(options.validate().is_err());
    assert!(ExtractionLimits::try_new(1, 1, 1, 2, 1, 1).is_err());
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL"]
async fn shutdown_joins_blocking_work_releases_the_pool_and_keeps_the_lease_recoverable(
    pool: PgPool,
) -> anyhow::Result<()> {
    use serde_json::json;
    crate::storage::note_index::configure_test_tags(&pool, &["Test"]).await?;
    let id = uuid::Uuid::new_v4();
    let body = br#"{"prompt":"[Test: synthetic shutdown]"}"#;
    sqlx::query("INSERT INTO request_traces(id, started_at, method, original_uri, upstream_url, request_kind, request_headers, request_body_bytes, request_body_compressed)
        VALUES ($1, now(), 'POST', '/completions', 'https://synthetic.invalid', 'generic_json', $2, $3, $4)")
        .bind(id).bind(json!({"content-type":"application/json"})).bind(body.len() as i64)
        .bind(zstd::stream::encode_all(body.as_slice(), 1)?).execute(&pool).await?;
    let index_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect_with(pool.connect_options().as_ref().clone())
        .await?;
    let observed_pool = index_pool.clone();
    let hooks = Arc::new(WorkerTestHooks {
        before_parse: std::sync::Barrier::new(2),
        release_parse: std::sync::Barrier::new(2),
    });
    let metrics = RuntimeMetrics::default();
    let worker = NoteIndexWorker::try_new(
        index_pool,
        ArchiveConfig::default(),
        NoteIndexOptions {
            test_hooks: Some(hooks.clone()),
            shutdown_timeout_secs: 1,
            ..Default::default()
        },
        metrics.clone(),
    )?;
    let started = hooks.clone();
    tokio::time::timeout(
        Duration::from_secs(5),
        tokio::task::spawn_blocking(move || {
            started.before_parse.wait();
        }),
    )
    .await??;
    let shutdown = tokio::time::timeout(Duration::from_secs(3), worker.shutdown()).await??;
    assert!(
        matches!(shutdown, NoteIndexShutdown::InProgress { .. }),
        "shutdown must return the owner of unfinished blocking work at its deadline"
    );
    let finished = tokio::spawn(shutdown.finish());
    tokio::task::spawn_blocking(move || {
        hooks.release_parse.wait();
    })
    .await?;
    let report = tokio::time::timeout(Duration::from_secs(5), finished).await???;
    assert_eq!(
        report.completed_tasks, 0,
        "cancelled blocking parse must not be published"
    );
    assert_eq!(
        observed_pool.size(),
        0,
        "worker closed its separate pool after joining"
    );
    let state: (String, bool) = sqlx::query_as("SELECT state, lease_token IS NOT NULL FROM request_prompt_index_state WHERE request_id = $1").bind(id).fetch_one(&pool).await?;
    assert_eq!(state, ("processing".into(), true));
    sqlx::query("UPDATE request_prompt_index_state SET lease_until = now() - interval '1 second' WHERE request_id = $1").bind(id).execute(&pool).await?;
    assert!(
        note_index_store::claim(&pool, crate::prompt_tags::NoteIndexVersion::V1, 60, 3)
            .await?
            .is_some()
    );
    Ok(())
}
