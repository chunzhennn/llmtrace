use super::*;
use crate::config::{ArchiveConfig, ArchiveStorageBackend};
use crate::metrics::RuntimeMetrics;
use crate::plugins::PluginManager;
use crate::storage;
use crate::storage::message_migration::{self, MessageMaintenanceAction as Action};
use crate::trace::{TraceEvent, build_trace};
use chrono::Utc;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

#[test]
fn identity_is_exact_utf8_and_batch_mapping_is_checked() {
    assert!(ContentHash::of("é") != ContentHash::of("e\u{301}"));
    assert!(ContentHash::of("") != ContentHash::of(" "));
    assert!(InternedMessages { ids: vec![] }.into_ids(1).is_err());
    assert!(MessageWriteMode::parse("future_mode").is_err());
}

async fn ingest(pool: &PgPool, key: &str) -> anyhow::Result<Uuid> {
    let plugins = PluginManager::load(&[], &Default::default(), &RuntimeMetrics::default())?;
    let mut event = TraceEvent::base(Uuid::new_v4(), Utc::now());
    event.original_uri = "/v1/chat/completions".into();
    event.upstream_url = "https://synthetic.invalid/v1/chat/completions".into();
    event.api_key_hash = Some(key.into());
    event.request_body = serde_json::to_vec(&json!({"messages": [
        {"role":"system", "content":"shared synthetic text"},
        {"role":"user", "content":"shared synthetic text"},
        {"role":"user", "content":""}
    ]}))?;
    event.request_body_bytes = event.request_body.len() as i64;
    let (trace, messages, uid, name) = build_trace(event, &plugins)?;
    let id = trace.id;
    storage::insert_trace(
        pool,
        &ArchiveConfig {
            storage_backend: ArchiveStorageBackend::Postgres,
            ..Default::default()
        },
        trace,
        messages,
        uid,
        name,
    )
    .await?;
    Ok(id)
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL"]
async fn modes_backfill_cleanup_and_rollback_preserve_all_preview_fields(
    pool: PgPool,
) -> anyhow::Result<()> {
    message_migration::run(&pool, Action::Legacy, 10, false).await?;
    let legacy = ingest(&pool, "legacy").await?;
    let measure = message_migration::run(&pool, Action::Measure, 10, false).await?;
    assert_eq!(
        (
            measure["occurrences"].as_i64(),
            measure["unique_contents"].as_i64()
        ),
        (Some(3), Some(2))
    );
    assert_eq!(
        (
            measure["preview_bytes"].as_i64(),
            measure["unique_preview_bytes"].as_i64()
        ),
        (Some(42), Some(21))
    );
    assert!(!serde_json::to_string(&measure)?.contains("shared synthetic text"));
    // Historical NULL remains unknown, not false, through all transitions.
    sqlx::query("UPDATE session_messages SET content_truncated = NULL WHERE request_id = $1")
        .bind(legacy)
        .execute(&pool)
        .await?;
    let before: Vec<(i64, String, String, Option<bool>)> = sqlx::query_as(
        "SELECT id, role, content, content_truncated FROM trace_messages ORDER BY id",
    )
    .fetch_all(&pool)
    .await?;
    message_migration::run(&pool, Action::DualWrite, 10, false).await?;
    ingest(&pool, "dual").await?;
    assert!(
        message_migration::run(&pool, Action::ReferencesOnly, 10, true)
            .await
            .is_err()
    );
    // One row per invocation proves resume works and the watermark cannot skip.
    for _ in &before {
        let result = message_migration::run(&pool, Action::Backfill, 1, false).await?;
        assert_eq!(result["affected_rows"], 1);
    }
    assert_eq!(
        message_migration::run(&pool, Action::Backfill, 1, false).await?["affected_rows"],
        0
    );
    assert!(
        message_migration::run(&pool, Action::ReferencesOnly, 10, false)
            .await
            .is_err()
    );
    message_migration::run(&pool, Action::ReferencesOnly, 10, true).await?;
    ingest(&pool, "reference").await?;
    assert!(
        message_migration::run(&pool, Action::Legacy, 10, false)
            .await
            .is_err()
    );
    message_migration::run(&pool, Action::ClearInline, 100, false).await?;
    let after: Vec<(i64, String, String, Option<bool>)> = sqlx::query_as("SELECT id, role, content, content_truncated FROM trace_messages WHERE request_id = $1 ORDER BY id").bind(legacy).fetch_all(&pool).await?;
    assert_eq!(before, after);
    let counts: (i64, i64, i64) = sqlx::query_as("SELECT (SELECT COUNT(*) FROM message_contents), (SELECT COUNT(*) FROM session_messages), (SELECT COUNT(*) FROM session_messages WHERE content IS NOT NULL)").fetch_one(&pool).await?;
    assert_eq!(
        counts,
        (2, 9, 0),
        "same text across roles and sessions shares only its body"
    );
    let session_id = sqlx::query_scalar("SELECT session_id FROM request_traces WHERE id = $1")
        .bind(legacy)
        .fetch_one(&pool)
        .await?;
    let detail = storage::get_session(&pool, session_id, None, None)
        .await?
        .unwrap();
    let page = storage::get_session(&pool, session_id, Some(1), Some(1))
        .await?
        .unwrap();
    assert_eq!(detail["messages"].as_array().unwrap().len(), 3);
    assert_eq!(page["messages"], json!([detail["messages"][1]]));
    assert_eq!(page["messages_page"]["has_more"], true);
    message_migration::run(&pool, Action::DualWrite, 10, false).await?;
    message_migration::run(&pool, Action::RestoreInline, 100, false).await?;
    message_migration::run(&pool, Action::Legacy, 10, false).await?;
    message_migration::run(&pool, Action::DiscardReferences, 100, false).await?;
    assert_eq!(
        message_migration::run(&pool, Action::Gc, 100, false).await?["affected_rows"],
        2
    );
    let restored: Vec<(i64, String, String, Option<bool>)> = sqlx::query_as("SELECT id, role, content, content_truncated FROM trace_messages WHERE request_id = $1 ORDER BY id").bind(legacy).fetch_all(&pool).await?;
    assert_eq!(before, restored);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL"]
async fn collisions_and_corrupt_dictionary_fail_without_overwriting(
    pool: PgPool,
) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    let hash = ContentHash::of("original");
    intern_texts(&mut tx, &[(hash, "original")]).await?;
    assert!(matches!(
        intern_texts(&mut tx, &[(hash, "different")]).await,
        Err(MessageStorageError::ContentMismatch)
    ));
    assert!(matches!(
        intern_texts(&mut tx, &[(hash, "original"), (hash, "different")]).await,
        Err(MessageStorageError::ContentMismatch)
    ));
    let content: String = sqlx::query_scalar("SELECT content FROM message_contents")
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(content, "original");
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL"]
async fn gc_skips_interned_rows_until_reference_commit_and_retains_shared_text(
    pool: PgPool,
) -> anyhow::Result<()> {
    let request = ingest(&pool, "gc").await?;
    let mut writer = pool.begin().await?;
    let id = intern_texts(
        &mut writer,
        &[(ContentHash::of("unreferenced"), "unreferenced")],
    )
    .await?
    .into_ids(1)?[0];
    writer.commit().await?;
    let mut writer = pool.begin().await?;
    intern_texts(
        &mut writer,
        &[(ContentHash::of("unreferenced"), "unreferenced")],
    )
    .await?;
    let mut gc = pool.begin().await?;
    assert_eq!(
        collect_unused(&mut gc, 100).await?,
        0,
        "GC skips KEY SHARE held by writer"
    );
    gc.commit().await?;
    sqlx::query("INSERT INTO session_messages(request_id, session_id, role, content_id, created_at) SELECT id, session_id, 'tool', $2, started_at FROM request_traces WHERE id = $1")
        .bind(request).bind(id).execute(&mut *writer).await?;
    writer.commit().await?;
    let mut gc = pool.begin().await?;
    assert_eq!(collect_unused(&mut gc, 100).await?, 0);
    gc.commit().await?;
    sqlx::query("DELETE FROM request_traces WHERE id = $1")
        .bind(request)
        .execute(&pool)
        .await?;
    let mut gc = pool.begin().await?;
    assert_eq!(collect_unused(&mut gc, 100).await?, 3);
    gc.commit().await?;
    let mut gc = pool.begin().await?;
    assert_eq!(collect_unused(&mut gc, 100).await?, 0);
    Ok(())
}

#[sqlx::test(migrations = false)]
#[ignore = "requires local PostgreSQL"]
async fn existing_v7_database_upgrades_additively_and_prepares_indexes_online(
    pool: PgPool,
) -> anyhow::Result<()> {
    for migration in [
        include_str!("../../../migrations/001_init.sql"),
        include_str!("../../../migrations/002_query_path_indexes.sql"),
        include_str!("../../../migrations/003_payload_archive_segments.sql"),
        include_str!("../../../migrations/004_llm_usage.sql"),
        include_str!("../../../migrations/005_archive_rotation.sql"),
        include_str!("../../../migrations/006_trace_journal.sql"),
        include_str!("../../../migrations/007_message_preview_truncation.sql"),
    ] {
        sqlx::raw_sql(migration).execute(&pool).await?;
    }
    let session = Uuid::new_v4();
    let request = Uuid::new_v4();
    sqlx::query("INSERT INTO trace_sessions(id, session_key, first_seen, last_seen) VALUES ($1, 'legacy-upgrade', now(), now())").bind(session).execute(&pool).await?;
    sqlx::query("INSERT INTO request_traces(id, started_at, method, original_uri, upstream_url, session_id) VALUES ($1, now(), 'POST', '/', 'https://synthetic.invalid', $2)").bind(request).bind(session).execute(&pool).await?;
    sqlx::query("INSERT INTO session_messages(request_id, session_id, role, content, created_at) VALUES ($1, $2, 'user', 'legacy exact text', now())").bind(request).bind(session).execute(&pool).await?;
    sqlx::raw_sql(include_str!("../../../migrations/008_message_contents.sql"))
        .execute(&pool)
        .await?;
    let text: String = sqlx::query_scalar("SELECT content FROM trace_messages")
        .fetch_one(&pool)
        .await?;
    assert_eq!(text, "legacy exact text");
    assert!(
        message_migration::run(&pool, Action::Backfill, 10, false)
            .await
            .is_err(),
        "existing tables must prepare outside the embedded transaction"
    );
    message_migration::run(&pool, Action::Prepare, 10, false).await?;
    message_migration::run(&pool, Action::Prepare, 10, false).await?;
    message_migration::run(&pool, Action::Backfill, 10, false).await?;
    message_migration::run(&pool, Action::ReferencesOnly, 10, true).await?;
    message_migration::run(&pool, Action::ClearInline, 10, false).await?;
    let preview: (String, Option<bool>) =
        sqlx::query_as("SELECT content, content_truncated FROM trace_messages")
            .fetch_one(&pool)
            .await?;
    assert_eq!(preview, (text, None));
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL"]
async fn backfill_rescans_late_commits_behind_its_high_water_mark(
    pool: PgPool,
) -> anyhow::Result<()> {
    message_migration::run(&pool, Action::Legacy, 10, false).await?;
    let request = ingest(&pool, "late").await?;
    let mut late = pool.begin().await?;
    let low: i64 = sqlx::query_scalar("INSERT INTO session_messages(request_id, session_id, role, content, created_at) SELECT id, session_id, 'user', 'late commit', now() FROM request_traces WHERE id = $1 RETURNING id")
        .bind(request).fetch_one(&mut *late).await?;
    let high: i64 = sqlx::query_scalar("INSERT INTO session_messages(request_id, session_id, role, content, created_at) SELECT id, session_id, 'user', 'early commit', now() FROM request_traces WHERE id = $1 RETURNING id")
        .bind(request).fetch_one(&pool).await?;
    assert!(high > low);
    message_migration::run(&pool, Action::DualWrite, 10, false).await?;
    message_migration::run(&pool, Action::Backfill, 100, false).await?;
    late.commit().await?;
    assert_eq!(
        message_migration::run(&pool, Action::Backfill, 100, false).await?["affected_rows"],
        1
    );
    assert_eq!(
        message_migration::run(&pool, Action::Status, 100, false).await?["integrity"]["missing_references"],
        0
    );
    Ok(())
}
