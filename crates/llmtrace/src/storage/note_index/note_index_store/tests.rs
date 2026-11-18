use super::*;
use crate::prompt_tags::{self, ExtractionLimits, RequestPromptMetadata};
use serde_json::json;
use std::sync::atomic::AtomicBool;

fn result(value: &str) -> TagExtraction {
    prompt_tags::extract(
        &serde_json::to_vec(&json!({"prompt":format!("[Test: {value}]")})).unwrap(),
        RequestPromptMetadata {
            request_kind: "generic_json",
            original_uri: "/completions",
            request_headers: &json!({"content-type":"application/json"}),
            capture_complete: true,
        },
        &["Test".into()],
        ExtractionLimits::default(),
        &AtomicBool::new(false),
    )
}

async fn seed(pool: &PgPool) -> anyhow::Result<Uuid> {
    crate::storage::note_index::configure_test_tags(pool, &["Test"]).await?;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO request_traces(id, started_at, method, original_uri, upstream_url) VALUES ($1, now(), 'POST', '/completions', 'https://synthetic.invalid')")
        .bind(id).execute(pool).await?;
    enqueue_missing(pool, NoteIndexVersion::V1, 100).await?;
    Ok(id)
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL"]
async fn expired_worker_cannot_replace_new_worker_results_or_recreate_deleted_sources(
    pool: PgPool,
) -> anyhow::Result<()> {
    let id = seed(&pool).await?;
    let old = claim(&pool, NoteIndexVersion::V1, 60, 3).await?.unwrap();
    assert!(claim(&pool, NoteIndexVersion::V1, 60, 3).await?.is_none());
    // Explicit database clock transition, no timing-dependent sleep.
    sqlx::query("UPDATE request_prompt_index_state SET lease_until = now() - interval '1 second' WHERE request_id = $1").bind(id).execute(&pool).await?;
    let new = claim(&pool, NoteIndexVersion::V1, 60, 3).await?.unwrap();
    assert_eq!(
        publish(&pool, new, result("new")).await?,
        PublishOutcome::Published
    );
    assert_eq!(
        publish(&pool, old, result("stale")).await?,
        PublishOutcome::LeaseLost
    );
    let notes: Vec<String> = sqlx::query_scalar("SELECT note_value FROM trace_prompt_notes")
        .fetch_all(&pool)
        .await?;
    assert_eq!(notes, ["new"]);
    let removed = seed(&pool).await?;
    let job = claim(&pool, NoteIndexVersion::V1, 60, 3).await?.unwrap();
    sqlx::query("DELETE FROM request_traces WHERE id = $1")
        .bind(removed)
        .execute(&pool)
        .await?;
    assert_eq!(
        publish(&pool, job, result("deleted")).await?,
        PublishOutcome::SourceRemoved
    );
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM request_prompt_index_state WHERE request_id = $1")
            .bind(removed)
            .fetch_one(&pool)
            .await?;
    assert_eq!(count, 0);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL"]
async fn exhausted_leases_do_not_hide_other_claimable_jobs(pool: PgPool) -> anyhow::Result<()> {
    let exhausted = seed(&pool).await?;
    let old = claim(&pool, NoteIndexVersion::V1, 60, 3).await?.unwrap();
    sqlx::query("UPDATE request_prompt_index_state SET attempt_count = 3, lease_until = now() - interval '1 second' WHERE request_id = $1")
        .bind(exhausted).execute(&pool).await?;
    let next = seed(&pool).await?;
    let job = claim(&pool, NoteIndexVersion::V1, 60, 3).await?.unwrap();
    assert_eq!(
        job.request_id(),
        next,
        "None must mean there is no claimable work"
    );
    let state: String =
        sqlx::query_scalar("SELECT state FROM request_prompt_index_state WHERE request_id = $1")
            .bind(exhausted)
            .fetch_one(&pool)
            .await?;
    assert_eq!(state, "failed");
    assert_eq!(
        publish(&pool, old, result("stale")).await?,
        PublishOutcome::LeaseLost
    );
    assert_eq!(
        publish(&pool, job, result("next")).await?,
        PublishOutcome::Published
    );
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL"]
async fn jobs_retry_finitely_and_dictionary_gc_follows_request_lifetime(
    pool: PgPool,
) -> anyhow::Result<()> {
    let id = seed(&pool).await?;
    let job = claim(&pool, NoteIndexVersion::V1, 60, 3).await?.unwrap();
    retry(&pool, job, 1).await?;
    assert!(claim(&pool, NoteIndexVersion::V1, 60, 3).await?.is_none());
    let state: String =
        sqlx::query_scalar("SELECT state FROM request_prompt_index_state WHERE request_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(state, "failed");
    for _ in 0..2 {
        seed(&pool).await?;
        let job = claim(&pool, NoteIndexVersion::V1, 60, 3).await?.unwrap();
        publish(&pool, job, result("shared")).await?;
    }
    assert_eq!(collect_unused(&pool, 100).await?, 0);
    let unique: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM note_values")
        .fetch_one(&pool)
        .await?;
    assert_eq!(unique, 1);
    sqlx::query("DELETE FROM request_traces")
        .execute(&pool)
        .await?;
    assert_eq!(collect_unused(&pool, 100).await?, 1);
    assert_eq!(collect_unused(&pool, 100).await?, 0);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL"]
async fn configured_tags_backfill_retained_input_and_hide_retired_results(
    pool: PgPool,
) -> anyhow::Result<()> {
    use crate::storage::note_index::{configure_test_tags, save_tag_settings, tag_settings};
    let version = NoteIndexVersion::V1;
    let original = tag_settings(&pool).await?;
    assert!(original.tags.is_empty());
    let id = Uuid::new_v4();
    let body = br#"{"prompt":"[Project: blue][severity: high][Project: green]"}"#;
    sqlx::query("INSERT INTO request_traces(id, started_at, method, original_uri, upstream_url, request_headers, request_body_bytes, request_body_compressed, response_body_compressed, response_body_truncated)
        VALUES ($1, now(), 'POST','/completions','https://synthetic.invalid',$2,$3,$4,$5,true)")
        .bind(id).bind(json!({"content-type":"application/json"})).bind(body.len() as i64)
        .bind(zstd::stream::encode_all(body.as_slice(),1)?).bind(b"corrupt response".as_slice()).execute(&pool).await?;
    assert_eq!(enqueue_missing(&pool, version, 1).await?, 0);
    for (tags, expected) in [
        (&["Project"][..], vec!["blue", "green"]),
        (&["severity"][..], vec!["high"]),
    ] {
        let config = configure_test_tags(&pool, tags).await?;
        assert_eq!(
            save_tag_settings(&pool, &config).await?.unwrap().revision,
            config.revision
        );
        assert!(save_tag_settings(&pool, &original).await?.is_none());
        let visible: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM trace_prompt_tags")
            .fetch_one(&pool)
            .await?;
        assert_eq!(visible, 0, "a retired config must not answer the new query");
        assert_eq!(enqueue_missing(&pool, version, 1).await?, 1);
        let job = claim(&pool, version, 60, 3).await?.unwrap();
        let source = load_source(&pool, &ArchiveConfig::default(), &job, 8192, 16384)
            .await?
            .unwrap();
        let extraction = prompt_tags::extract(
            &source.body.unwrap(),
            RequestPromptMetadata {
                request_kind: &source.metadata.summary.request_kind,
                original_uri: &source.metadata.summary.original_uri,
                request_headers: &source.metadata.request_headers,
                capture_complete: !source.metadata.summary.request_body_truncated,
            },
            &job.tags,
            ExtractionLimits::default(),
            &AtomicBool::new(false),
        );
        assert_eq!(extraction.state, ExtractionState::Ready);
        publish(&pool, job, extraction).await?;
        let values: Vec<String> = sqlx::query_scalar(
            "SELECT tag_value FROM trace_prompt_tags WHERE tag_name = $1 ORDER BY match_ordinal",
        )
        .bind(tags[0])
        .fetch_all(&pool)
        .await?;
        assert_eq!(values, expected);
    }
    configure_test_tags(&pool, &["Test"]).await?;
    assert_eq!(enqueue_missing(&pool, version, 1).await?, 1);
    let in_flight = claim(&pool, version, 60, 3).await?.unwrap();
    configure_test_tags(&pool, &[]).await?;
    assert_eq!(
        publish(&pool, in_flight, result("late")).await?,
        PublishOutcome::LeaseLost
    );
    assert_eq!(enqueue_missing(&pool, version, 10).await?, 0);
    assert!(claim(&pool, version, 60, 3).await?.is_none());
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL"]
async fn historical_shared_segments_respect_the_derived_index_decode_budget(
    pool: PgPool,
) -> anyhow::Result<()> {
    use crate::config::ArchiveStorageBackend;
    use crate::plugins::PluginManager;
    use crate::trace::{TraceEvent, build_trace};
    crate::storage::note_index::configure_test_tags(&pool, &["Test"]).await?;
    let plugins = PluginManager::load(
        &[],
        &Default::default(),
        &crate::metrics::RuntimeMetrics::default(),
    )?;
    let archive = ArchiveConfig {
        storage_backend: ArchiveStorageBackend::Postgres,
        ..Default::default()
    };
    let mut event = TraceEvent::base(Uuid::new_v4(), Utc::now());
    event.original_uri = "/v1/chat/completions".into();
    event.upstream_url = "https://synthetic.invalid/v1/chat/completions".into();
    event.request_headers = json!({"content-type":"application/json"});
    event.request_body =
        br#"{"messages":[{"role":"user","content":"[Test: small request]"}]}"#.to_vec();
    event.response_body = vec![b'x'; 64 * 1024];
    event.request_body_bytes = event.request_body.len() as i64;
    event.response_body_bytes = event.response_body.len() as i64;
    let (trace, messages, uid, name) = build_trace(event, &plugins)?;
    crate::storage::insert_trace(&pool, &archive, trace, messages, uid, name).await?;
    let job = claim(&pool, NoteIndexVersion::V1, 60, 3).await?.unwrap();
    let source = load_source(&pool, &archive, &job, 8192, 16384)
        .await?
        .unwrap();
    let result = source.unavailable.unwrap();
    assert!(source.body.is_none());
    assert_eq!(result.state, ExtractionState::Partial);
    assert!(result.diagnostics.contains(&NoteDiagnostic::BodyBudget));
    Ok(())
}
