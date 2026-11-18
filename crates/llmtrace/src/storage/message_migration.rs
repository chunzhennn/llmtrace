//! Explicit, restartable preview migration commands. No automatic inline cleanup.
//! Each invocation is bounded; committed progress survives cancellation or restart.
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgPool, Postgres, Row, Transaction};

use super::message_contents;
use super::message_contents::{ContentHash, MessageWriteMode};

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub(crate) enum MessageMaintenanceAction {
    Prepare,
    Status,
    Measure,
    Backfill,
    DualWrite,
    ReferencesOnly,
    ClearInline,
    RestoreInline,
    Legacy,
    DiscardReferences,
    Gc,
}

#[derive(Serialize, sqlx::FromRow)]
pub(crate) struct MessageIntegrity {
    missing_references: i64,
    dangling_references: i64,
    mismatched_contents: i64,
    missing_inline: i64,
}

impl MessageIntegrity {
    fn references_verified(&self) -> bool {
        self.missing_references == 0
            && self.dangling_references == 0
            && self.mismatched_contents == 0
    }
}

#[derive(Serialize, sqlx::FromRow)]
pub(super) struct MessageVerification {
    checked_at: Option<DateTime<Utc>>,
    missing_references: Option<i64>,
    dangling_references: Option<i64>,
    mismatched_contents: Option<i64>,
    missing_inline: Option<i64>,
}

pub(super) async fn verification_snapshot(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<MessageVerification, sqlx::Error> {
    sqlx::query_as("SELECT verified_at AS checked_at, verified_missing_references AS missing_references,
        verified_dangling_references AS dangling_references, verified_mismatched_contents AS mismatched_contents,
        verified_missing_inline AS missing_inline FROM message_storage_migration WHERE singleton")
        .fetch_one(&mut **tx).await
}

async fn record_verification(
    tx: &mut Transaction<'_, Postgres>,
    check: &MessageIntegrity,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE message_storage_migration SET verified_at = now(), verified_missing_references = $1,
        verified_dangling_references = $2, verified_mismatched_contents = $3, verified_missing_inline = $4 WHERE singleton")
        .bind(check.missing_references).bind(check.dangling_references).bind(check.mismatched_contents).bind(check.missing_inline).execute(&mut **tx).await?;
    Ok(())
}

pub(super) async fn integrity(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<MessageIntegrity, sqlx::Error> {
    sqlx::query_as(
        "SELECT COUNT(*) FILTER (WHERE m.content_id IS NULL)::bigint AS missing_references,
                COUNT(*) FILTER (WHERE m.content_id IS NOT NULL AND c.id IS NULL)::bigint AS dangling_references,
                COUNT(*) FILTER (WHERE m.content IS NOT NULL AND c.id IS NOT NULL AND
                    (m.content <> c.content OR octet_length(m.content) <> c.content_bytes))::bigint AS mismatched_contents,
                COUNT(*) FILTER (WHERE m.content IS NULL)::bigint AS missing_inline
         FROM session_messages m LEFT JOIN message_contents c ON c.id = m.content_id",
    ).fetch_one(&mut **tx).await
}

#[derive(Serialize)]
struct MigrationStatus {
    write_mode: MessageWriteMode,
    high_water_id: i64,
    backfilled_rows: i64,
    updated_at: DateTime<Utc>,
    last_error: Option<String>,
    integrity: Option<MessageIntegrity>,
    affected_rows: u64,
}

/// One batch only. The caller controls pacing and checks available database/WAL
/// space before every batch; no SKIP LOCKED watermark can skip late commits.
pub(crate) async fn run(
    pool: &PgPool,
    action: MessageMaintenanceAction,
    batch_size: i64,
    gates_acknowledged: bool,
) -> anyhow::Result<serde_json::Value> {
    anyhow::ensure!(
        (1..=10_000).contains(&batch_size),
        "message batch size must be 1..=10000"
    );
    let result = run_inner(pool, action, batch_size, gates_acknowledged).await;
    if result.is_err() {
        // Keep a stable diagnostic without persisting body text or SQL parameters.
        let _ = sqlx::query("UPDATE message_storage_migration SET last_error = 'batch_failed', updated_at = now() WHERE singleton")
            .execute(pool).await;
    }
    result
}

async fn run_inner(
    pool: &PgPool,
    action: MessageMaintenanceAction,
    batch_size: i64,
    gates_acknowledged: bool,
) -> anyhow::Result<serde_json::Value> {
    use MessageMaintenanceAction as Action;
    if matches!(action, Action::Prepare) {
        prepare_online(pool).await?;
        return Ok(serde_json::json!({"prepared":true, "next_action":"status"}));
    }
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL statement_timeout = '30s'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL lock_timeout = '2s'")
        .execute(&mut *tx)
        .await?;
    if matches!(action, Action::Measure) {
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let measurement = measure(&mut tx).await?;
        tx.commit().await?;
        return Ok(measurement);
    }
    // Serialize maintenance and fence updated writers while validating switches.
    let mode: String = sqlx::query_scalar(
        "SELECT write_mode FROM message_storage_migration WHERE singleton FOR UPDATE",
    )
    .fetch_one(&mut *tx)
    .await?;
    let mode = MessageWriteMode::parse(&mode)?;
    let mut affected_rows = 0;
    match action {
        Action::Prepare => unreachable!("online preparation handled before transaction"),
        Action::Status | Action::Measure => {}
        Action::Backfill => {
            validate_schema_ready(&mut tx).await?;
            anyhow::ensure!(
                mode != MessageWriteMode::Legacy,
                "enable dual-write before backfill"
            );
            let rows = sqlx::query("SELECT id, content FROM session_messages WHERE content_id IS NULL ORDER BY id LIMIT $1 FOR UPDATE")
                .bind(batch_size).fetch_all(&mut *tx).await?;
            let mut message_ids = Vec::with_capacity(rows.len());
            let mut inputs = Vec::with_capacity(rows.len());
            for row in &rows {
                message_ids.push(row.try_get::<i64, _>("id")?);
                let content: &str = row.try_get("content")?;
                inputs.push((ContentHash::of(content), content));
            }
            let content_ids = message_contents::intern_texts(&mut tx, &inputs)
                .await?
                .into_ids(rows.len())?;
            affected_rows = sqlx::query("UPDATE session_messages m SET content_id = batch.content_id FROM UNNEST($1::bigint[], $2::bigint[]) AS batch(id, content_id) WHERE m.id = batch.id")
                .bind(&message_ids).bind(&content_ids).execute(&mut *tx).await?.rows_affected();
            anyhow::ensure!(
                affected_rows == rows.len() as u64,
                "message backfill batch mapping changed"
            );
            sqlx::query("UPDATE message_storage_migration SET high_water_id = GREATEST(high_water_id, $1), backfilled_rows = backfilled_rows + $2 WHERE singleton")
                .bind(message_ids.last().copied().unwrap_or(0)).bind(i64::try_from(affected_rows)?).execute(&mut *tx).await?;
        }
        Action::DualWrite | Action::ReferencesOnly | Action::Legacy => {
            let target = match action {
                Action::DualWrite => MessageWriteMode::DualWrite,
                Action::ReferencesOnly => MessageWriteMode::ReferencesOnly,
                Action::Legacy => MessageWriteMode::Legacy,
                _ => unreachable!("mode action selected above"),
            };
            validate_switch(&mut tx, target, gates_acknowledged).await?;
            sqlx::query("UPDATE message_storage_migration SET write_mode = $1, gates_acknowledged_at = CASE WHEN $2 THEN now() ELSE gates_acknowledged_at END WHERE singleton")
                .bind(target.as_str()).bind(gates_acknowledged).execute(&mut *tx).await?;
        }
        Action::ClearInline => {
            anyhow::ensure!(
                mode == MessageWriteMode::ReferencesOnly,
                "inline cleanup requires references-only mode and verified rollout gates"
            );
            // Every affected row is compared again, inside the cleanup transaction.
            affected_rows = sqlx::query("UPDATE session_messages m SET content = NULL WHERE id IN (
                SELECT m.id FROM session_messages m JOIN message_contents c ON c.id = m.content_id
                WHERE m.content IS NOT NULL AND m.content = c.content AND octet_length(m.content) = c.content_bytes
                ORDER BY m.id LIMIT $1 FOR UPDATE OF m)")
                .bind(batch_size).execute(&mut *tx).await?.rows_affected();
        }
        Action::RestoreInline => {
            anyhow::ensure!(
                mode != MessageWriteMode::ReferencesOnly,
                "switch to dual-write before restoring inline data"
            );
            affected_rows = sqlx::query("UPDATE session_messages m SET content = c.content FROM message_contents c WHERE m.content_id = c.id AND m.id IN (
                SELECT id FROM session_messages WHERE content IS NULL ORDER BY id LIMIT $1 FOR UPDATE)")
                .bind(batch_size).execute(&mut *tx).await?.rows_affected();
        }
        Action::DiscardReferences => {
            anyhow::ensure!(
                mode == MessageWriteMode::Legacy,
                "restore and validate inline data, then switch to legacy before discarding references"
            );
            affected_rows = sqlx::query("UPDATE session_messages SET content_id = NULL WHERE id IN (
                SELECT id FROM session_messages WHERE content_id IS NOT NULL AND content IS NOT NULL ORDER BY id LIMIT $1 FOR UPDATE)")
                .bind(batch_size).execute(&mut *tx).await?.rows_affected();
        }
        Action::Gc => {
            affected_rows = message_contents::collect_unused(&mut tx, batch_size).await?;
        }
    }
    if !matches!(action, Action::Status) {
        sqlx::query("UPDATE message_storage_migration SET updated_at = now(), last_error = NULL WHERE singleton")
            .execute(&mut *tx).await?;
    }
    let row = sqlx::query("SELECT write_mode, high_water_id, backfilled_rows, updated_at, last_error FROM message_storage_migration WHERE singleton")
        .fetch_one(&mut *tx).await?;
    let check = if matches!(action, Action::Status) {
        let check = integrity(&mut tx).await?;
        record_verification(&mut tx, &check).await?;
        Some(check)
    } else {
        None
    };
    let status = MigrationStatus {
        write_mode: MessageWriteMode::parse(row.try_get("write_mode")?)?,
        high_water_id: row.try_get("high_water_id")?,
        backfilled_rows: row.try_get("backfilled_rows")?,
        updated_at: row.try_get("updated_at")?,
        last_error: row.try_get("last_error")?,
        integrity: check,
        affected_rows,
    };
    tx.commit().await?;
    Ok(serde_json::to_value(status)?)
}

async fn prepare_online(pool: &PgPool) -> anyhow::Result<()> {
    // Each statement owns its connection and is outside a transaction. A failed
    // concurrent build can leave an invalid index; repair only our named index.
    for (name, create, drop_invalid) in [
        (
            "idx_session_messages_content_id",
            "CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_session_messages_content_id ON session_messages(content_id) WHERE content_id IS NOT NULL",
            "DROP INDEX CONCURRENTLY IF EXISTS idx_session_messages_content_id",
        ),
        (
            "idx_session_messages_uninterned",
            "CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_session_messages_uninterned ON session_messages(id) WHERE content_id IS NULL",
            "DROP INDEX CONCURRENTLY IF EXISTS idx_session_messages_uninterned",
        ),
    ] {
        let valid: Option<bool> = sqlx::query_scalar(
            "SELECT indisvalid FROM pg_index WHERE indexrelid = to_regclass($1)",
        )
        .bind(name)
        .fetch_optional(pool)
        .await?;
        if valid == Some(false) {
            sqlx::query(drop_invalid).execute(pool).await?;
        }
        sqlx::query(create).execute(pool).await?;
    }
    sqlx::query("ALTER TABLE session_messages VALIDATE CONSTRAINT session_messages_content_fk")
        .execute(pool)
        .await?;
    sqlx::query(
        "ALTER TABLE session_messages VALIDATE CONSTRAINT session_messages_content_present",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn validate_switch(
    tx: &mut Transaction<'_, Postgres>,
    target: MessageWriteMode,
    gates_acknowledged: bool,
) -> anyhow::Result<()> {
    let integrity = integrity(tx).await?;
    record_verification(tx, &integrity).await?;
    anyhow::ensure!(
        integrity.dangling_references == 0 && integrity.mismatched_contents == 0,
        "message dictionary integrity check failed"
    );
    match target {
        MessageWriteMode::Legacy => anyhow::ensure!(
            integrity.missing_inline == 0,
            "restore all inline content before legacy mode or an old binary rollback"
        ),
        MessageWriteMode::DualWrite => {}
        MessageWriteMode::ReferencesOnly => {
            anyhow::ensure!(
                gates_acknowledged,
                "references-only requires --acknowledge-migration-gates (all writers upgraded; measured storage, throughput and export gates passed)"
            );
            anyhow::ensure!(
                integrity.references_verified(),
                "finish backfill and resolve integrity mismatches before references-only mode"
            );
            validate_schema_ready(tx).await?;
        }
    }
    Ok(())
}

async fn validate_schema_ready(tx: &mut Transaction<'_, Postgres>) -> anyhow::Result<()> {
    let ready: bool = sqlx::query_scalar("SELECT
        (SELECT COUNT(*) = 2 AND bool_and(convalidated) FROM pg_constraint WHERE conrelid = 'session_messages'::regclass AND conname IN ('session_messages_content_fk', 'session_messages_content_present'))
        AND (SELECT COUNT(*) = 2 AND bool_and(indisvalid) FROM pg_index WHERE indexrelid IN (to_regclass('idx_session_messages_content_id'), to_regclass('idx_session_messages_uninterned')))")
        .fetch_one(&mut **tx).await?;
    anyhow::ensure!(
        ready,
        "run --message-maintenance prepare before backfill or references-only mode"
    );
    Ok(())
}

/// Exact logical deduplication measurements, distinct from PostgreSQL disk sizes.
/// Includes no prompt text. This potentially expensive offline command is explicit.
async fn measure(tx: &mut Transaction<'_, Postgres>) -> anyhow::Result<serde_json::Value> {
    let row = sqlx::query("WITH unique_text AS (SELECT content, COUNT(*)::bigint AS occurrences FROM trace_messages GROUP BY content)
        SELECT COUNT(*)::bigint AS unique_contents,
               COALESCE(SUM(occurrences), 0)::bigint AS occurrences,
               COALESCE(SUM(octet_length(content)::bigint * occurrences), 0)::bigint AS preview_bytes,
               COALESCE(SUM(octet_length(content)::bigint), 0)::bigint AS unique_preview_bytes,
               COUNT(*) FILTER (WHERE octet_length(content) <= 64)::bigint AS short_unique_contents FROM unique_text")
        .fetch_one(&mut **tx).await?;
    let mut result = serde_json::Map::new();
    for name in [
        "unique_contents",
        "occurrences",
        "preview_bytes",
        "unique_preview_bytes",
        "short_unique_contents",
    ] {
        result.insert(name.to_owned(), row.try_get::<i64, _>(name)?.into());
    }
    for (name, query) in [
        (
            "message_storage_bytes",
            "SELECT (pg_total_relation_size('session_messages') + pg_total_relation_size('message_contents'))::bigint",
        ),
        (
            "database_bytes",
            "SELECT pg_database_size(current_database())::bigint",
        ),
    ] {
        let bytes: i64 = sqlx::query_scalar(query).fetch_one(&mut **tx).await?;
        result.insert(name.to_owned(), bytes.into());
    }
    // Reuse the admin storage projection; physical/statistical counters are
    // observations, not MVCC-consistent logical row totals or reclaimed space.
    let relations = sqlx::query(super::retention::STORAGE_SUMMARY_SQL)
        .fetch_all(&mut **tx)
        .await?;
    let mut sizes = Vec::with_capacity(relations.len());
    for relation in relations {
        sizes.push(serde_json::json!({
            "name": relation.try_get::<String, _>("name")?,
            "heap_bytes": relation.try_get::<i64, _>("table_bytes")?,
            "index_bytes": relation.try_get::<i64, _>("index_bytes")?,
            "toast_and_auxiliary_bytes": relation.try_get::<i64, _>("auxiliary_bytes")?,
            "total_bytes": relation.try_get::<i64, _>("total_bytes")?,
            "dead_rows_estimate": relation.try_get::<i64, _>("dead_rows_estimate")?
        }));
    }
    result.insert("relations".to_owned(), serde_json::to_value(sizes)?);
    for (name, query) in [
        ("requests", "SELECT jsonb_build_object('count', COUNT(*), 'first_seen', MIN(started_at), 'last_seen', MAX(started_at),
            'captured_request_bytes', COALESCE(SUM(request_body_bytes), 0), 'captured_response_bytes', COALESCE(SUM(response_body_bytes), 0),
            'request_bytes_p50_p95_p99', percentile_disc(ARRAY[0.5,0.95,0.99]) WITHIN GROUP (ORDER BY request_body_bytes),
            'truncated_requests', COUNT(*) FILTER (WHERE request_body_truncated)) FROM request_traces"),
        ("request_kinds", "SELECT COALESCE(jsonb_object_agg(kind, n), '{}'::jsonb) FROM (SELECT request_kind AS kind, COUNT(*) AS n FROM request_traces GROUP BY request_kind) grouped"),
        ("session_lengths", "SELECT jsonb_build_object('sessions_with_requests', COUNT(*), 'requests_per_session_p50_p95_p99',
            percentile_disc(ARRAY[0.5,0.95,0.99]) WITHIN GROUP (ORDER BY n), 'max_requests_per_session', MAX(n)) FROM
            (SELECT COUNT(*) AS n FROM request_traces WHERE session_id IS NOT NULL GROUP BY session_id) lengths"),
        ("roles", "SELECT COALESCE(jsonb_object_agg(role, n), '{}'::jsonb) FROM (SELECT CASE WHEN role IN ('system','developer','user','assistant','tool') THEN role ELSE 'other' END AS role,
            COUNT(*) AS n FROM session_messages GROUP BY 1) grouped"),
        ("note_index", "WITH hits AS (SELECT COUNT(*) AS occurrences, COUNT(DISTINCT note_value_id) AS unique_values,
            MAX(octet_length(tag_value)) AS max_bytes FROM trace_prompt_tags)
            SELECT jsonb_build_object('tag_revision', s.tag_revision, 'indexed_requests', COUNT(i.request_id), 'external_history', COUNT(*) FILTER (WHERE external_history),
            'incomplete_capture', COUNT(*) FILTER (WHERE NOT capture_complete), 'partial', COUNT(*) FILTER (WHERE state = 'partial'),
            'unsupported', COUNT(*) FILTER (WHERE state = 'unsupported'), 'failed', COUNT(*) FILTER (WHERE state = 'failed'),
            'note_occurrences', (SELECT occurrences FROM hits), 'unique_values', (SELECT unique_values FROM hits),
            'max_value_bytes', (SELECT max_bytes FROM hits))
            FROM note_index_settings s LEFT JOIN request_prompt_index_state i ON i.index_version = s.active_version
            AND i.tag_revision = s.tag_revision AND cardinality(s.tag_names) > 0 WHERE s.singleton GROUP BY s.tag_revision"),
        ("wal_server_global", "SELECT jsonb_build_object('wal_records', wal_records, 'wal_bytes', wal_bytes, 'stats_reset', stats_reset,
            'scope', 'server_global_cumulative_not_database_or_command_delta') FROM pg_stat_wal"),
    ] {
        let measurement: Option<serde_json::Value> = sqlx::query_scalar(query)
            .fetch_optional(&mut **tx)
            .await?;
        result.insert(name.to_owned(), measurement.unwrap_or(serde_json::Value::Null));
    }
    result.insert("measured_at".to_owned(), serde_json::to_value(Utc::now())?);
    result.insert(
        "scope".to_owned(),
        "retained_message_previews_exact_utf8".into(),
    );
    Ok(result.into())
}
