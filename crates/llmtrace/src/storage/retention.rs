//! Age retention and storage administration.
use super::*;

pub(super) const RETENTION_STATUS_SQL: &str = r#"
        SELECT CASE
                   WHEN $1::timestamptz IS NULL THEN 0::bigint
                   ELSE (
                       SELECT COUNT(*)::bigint
                       FROM request_traces
                       WHERE started_at < $1
                   )
               END AS request_traces,
               CASE
                   WHEN $1::timestamptz IS NULL THEN 0::bigint
                   ELSE (
                       SELECT COUNT(*)::bigint
                       FROM trace_rollups_minute
                       WHERE bucket < $1
                   )
               END AS trace_rollups_minute,
               CASE
                   WHEN $1::timestamptz IS NULL THEN 0::bigint
                   ELSE (
                       SELECT COUNT(*)::bigint
                       FROM trace_sessions s
                       WHERE s.last_seen < $1
                         AND NOT EXISTS (
                             SELECT 1
                             FROM request_traces r
                             WHERE r.session_id = s.id
                         )
                   )
               END AS trace_sessions,
               CASE
                   WHEN $1::timestamptz IS NULL THEN 0::bigint
                   ELSE (
                       SELECT COUNT(*)::bigint
                       FROM ui_audit_events
                       WHERE created_at < $1
                   )
               END AS ui_audit_events,
               (
                   SELECT COUNT(*)::bigint
                   FROM ui_sessions
                   WHERE expires_at < $2
               ) AS ui_sessions,
               (
                   SELECT COUNT(*)::bigint
                   FROM oauth_states
                   WHERE expires_at < $2
               ) AS oauth_states
        "#;
pub(super) const STORAGE_SUMMARY_SQL: &str = r#"
        WITH tracked_relations(display_order, name) AS (
            VALUES
                (1, 'request_traces'),
                (2, 'payload_archive_segments'),
                (3, 'payload_archive_segment_blobs'),
                (4, 'payload_archive_records'),
                (5, 'trace_rollups_minute'),
                (6, 'trace_sessions'),
                (7, 'session_messages'),
                (8, 'ui_audit_events'),
                (9, 'ui_sessions'),
                (10, 'oauth_states'),
                (11, 'archive_file_deletions')
        ),
        current_schema_oid AS (
            SELECT oid
            FROM pg_namespace
            WHERE nspname = current_schema()
        )
        SELECT t.name,
               c.oid IS NOT NULL AS present,
               GREATEST(COALESCE(c.reltuples, 0)::bigint, 0)::bigint AS estimated_rows,
               COALESCE(s.n_live_tup, 0)::bigint AS live_rows_estimate,
               COALESCE(s.n_dead_tup, 0)::bigint AS dead_rows_estimate,
               COALESCE(pg_total_relation_size(c.oid), 0)::bigint AS total_bytes,
               COALESCE(pg_relation_size(c.oid), 0)::bigint AS table_bytes,
               COALESCE(pg_indexes_size(c.oid), 0)::bigint AS index_bytes,
               GREATEST(
                   COALESCE(pg_total_relation_size(c.oid), 0)
                   - COALESCE(pg_relation_size(c.oid), 0)
                   - COALESCE(pg_indexes_size(c.oid), 0),
                   0
               )::bigint AS auxiliary_bytes,
               s.last_vacuum AS last_vacuum_at,
               s.last_autovacuum AS last_autovacuum_at,
               s.last_analyze AS last_analyze_at,
               s.last_autoanalyze AS last_autoanalyze_at
        FROM tracked_relations t
        CROSS JOIN current_schema_oid n
        LEFT JOIN pg_class c
          ON c.relnamespace = n.oid
         AND c.relname = t.name
         AND c.relkind IN ('r', 'p')
        LEFT JOIN pg_stat_user_tables s ON s.relid = c.oid
        ORDER BY t.display_order
        "#;
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetentionPruneResult {
    pub request_traces: u64,
    pub trace_rollups_minute: u64,
    pub trace_sessions: u64,
    pub ui_audit_events: u64,
    pub ui_sessions: u64,
    pub oauth_states: u64,
}

impl RetentionPruneResult {
    pub fn total_deleted(&self) -> u64 {
        self.request_traces
            + self.trace_rollups_minute
            + self.trace_sessions
            + self.ui_audit_events
            + self.ui_sessions
            + self.oauth_states
    }
}

pub fn spawn_retention_pruner(
    pool: PgPool,
    config: StorageConfig,
    metrics: RuntimeMetrics,
) -> Option<JoinHandle<()>> {
    let retention_days = config.retention_days?;
    let interval = Duration::from_secs(config.retention_prune_interval_secs);
    let batch_size = config.retention_prune_batch_size;

    Some(tokio::spawn(async move {
        loop {
            match prune_retention(&pool, retention_days, batch_size).await {
                Ok(result) => {
                    metrics.retention_succeeded(&result);
                    if result.total_deleted() > 0 {
                        tracing::info!(
                            request_traces = result.request_traces,
                            trace_rollups_minute = result.trace_rollups_minute,
                            trace_sessions = result.trace_sessions,
                            ui_audit_events = result.ui_audit_events,
                            ui_sessions = result.ui_sessions,
                            oauth_states = result.oauth_states,
                            "retention prune completed"
                        );
                    } else {
                        tracing::debug!("retention prune completed with no expired rows");
                    }
                }
                Err(error) => {
                    metrics.retention_failed(error.to_string());
                    tracing::warn!(%error, "retention prune failed");
                }
            }
            tokio::time::sleep(interval).await;
        }
    }))
}

pub async fn prune_retention(
    pool: &PgPool,
    retention_days: i64,
    batch_size: i64,
) -> anyhow::Result<RetentionPruneResult> {
    if retention_days <= 0 {
        anyhow::bail!("retention_days must be greater than 0");
    }
    if batch_size <= 0 {
        anyhow::bail!("retention prune batch_size must be greater than 0");
    }

    let now = Utc::now();
    let cutoff = retention_cutoff(now, retention_days);
    let request_traces = delete_request_traces_before(pool, cutoff, batch_size).await?;
    let trace_rollups_minute = delete_rollups_before(pool, cutoff, batch_size).await?;
    let trace_sessions = delete_empty_sessions_before(pool, cutoff, batch_size).await?;
    let ui_audit_events = delete_ui_audit_events_before(pool, cutoff, batch_size).await?;
    let ui_sessions = delete_expired_ui_sessions(pool, now, batch_size).await?;
    let oauth_states = delete_expired_oauth_states(pool, now, batch_size).await?;

    Ok(RetentionPruneResult {
        request_traces,
        trace_rollups_minute,
        trace_sessions,
        ui_audit_events,
        ui_sessions,
        oauth_states,
    })
}

pub async fn retention_status(pool: &PgPool, config: &StorageConfig) -> anyhow::Result<Value> {
    let retention_days = config.retention_days;
    if retention_days.is_some_and(|days| days <= 0) {
        anyhow::bail!("retention_days must be greater than 0 when set");
    }

    let now = Utc::now();
    let cutoff = retention_days.map(|days| retention_cutoff(now, days));
    let mut tx = begin_api_read_tx(pool).await?;
    let row = sqlx::query(RETENTION_STATUS_SQL)
        .bind(cutoff)
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;
    let footprint = rotation::archive_footprint(&mut tx).await?;
    tx.commit().await?;

    let request_traces = row.get::<i64, _>("request_traces");
    let trace_rollups_minute = row.get::<i64, _>("trace_rollups_minute");
    let trace_sessions = row.get::<i64, _>("trace_sessions");
    let ui_audit_events = row.get::<i64, _>("ui_audit_events");
    let ui_sessions = row.get::<i64, _>("ui_sessions");
    let oauth_states = row.get::<i64, _>("oauth_states");

    Ok(json!({
        "enabled": retention_days.is_some(),
        "retention_days": retention_days,
        "cutoff": cutoff,
        "checked_at": now,
        "prune_interval_secs": config.retention_prune_interval_secs,
        "prune_batch_size": config.retention_prune_batch_size,
        "rotation": {
            "enabled": config.rotate_size_bytes > 0,
            "size_bytes": config.rotate_size_bytes,
            "check_interval_secs": config.rotate_check_interval_secs,
            "retained_bytes": footprint.retained_bytes,
            "pending_delete_bytes": footprint.pending_bytes,
            "pending_delete_files": footprint.pending_files,
            "over_limit": config.rotate_size_bytes > 0
                && footprint.retained_bytes.saturating_add(footprint.pending_bytes) as u64 > config.rotate_size_bytes,
        },
        "expired": {
            "request_traces": request_traces,
            "trace_rollups_minute": trace_rollups_minute,
            "trace_sessions": trace_sessions,
            "ui_audit_events": ui_audit_events,
            "ui_sessions": ui_sessions,
            "oauth_states": oauth_states,
            "total": retention_expired_total([
                request_traces,
                trace_rollups_minute,
                trace_sessions,
                ui_audit_events,
                ui_sessions,
                oauth_states,
            ]),
        },
    }))
}

pub async fn storage_summary(pool: &PgPool) -> anyhow::Result<Value> {
    let checked_at = Utc::now();
    let mut tx = begin_api_read_tx(pool).await?;
    let rows = sqlx::query(STORAGE_SUMMARY_SQL).fetch_all(&mut *tx).await?;
    tx.commit().await?;

    let mut total_bytes = 0;
    let mut table_bytes = 0;
    let mut index_bytes = 0;
    let mut auxiliary_bytes = 0;
    let mut relation_count = 0;
    let mut present_relation_count = 0;
    let mut relations = Vec::with_capacity(rows.len());

    for row in rows {
        relation_count += 1;
        let present = row.get::<bool, _>("present");
        if present {
            present_relation_count += 1;
        }
        let row_total_bytes = nonnegative_i64(row.get::<i64, _>("total_bytes"));
        let row_table_bytes = nonnegative_i64(row.get::<i64, _>("table_bytes"));
        let row_index_bytes = nonnegative_i64(row.get::<i64, _>("index_bytes"));
        let row_auxiliary_bytes = nonnegative_i64(row.get::<i64, _>("auxiliary_bytes"));

        total_bytes = saturating_add_i64(total_bytes, row_total_bytes);
        table_bytes = saturating_add_i64(table_bytes, row_table_bytes);
        index_bytes = saturating_add_i64(index_bytes, row_index_bytes);
        auxiliary_bytes = saturating_add_i64(auxiliary_bytes, row_auxiliary_bytes);

        relations.push(json!({
            "name": row.get::<String, _>("name"),
            "present": present,
            "estimated_rows": nonnegative_i64(row.get::<i64, _>("estimated_rows")),
            "live_rows_estimate": nonnegative_i64(row.get::<i64, _>("live_rows_estimate")),
            "dead_rows_estimate": nonnegative_i64(row.get::<i64, _>("dead_rows_estimate")),
            "total_bytes": row_total_bytes,
            "table_bytes": row_table_bytes,
            "index_bytes": row_index_bytes,
            "auxiliary_bytes": row_auxiliary_bytes,
            "last_vacuum_at": row.try_get::<Option<DateTime<Utc>>, _>("last_vacuum_at")?,
            "last_autovacuum_at": row.try_get::<Option<DateTime<Utc>>, _>("last_autovacuum_at")?,
            "last_analyze_at": row.try_get::<Option<DateTime<Utc>>, _>("last_analyze_at")?,
            "last_autoanalyze_at": row.try_get::<Option<DateTime<Utc>>, _>("last_autoanalyze_at")?,
        }));
    }

    Ok(json!({
        "checked_at": checked_at,
        "totals": {
            "relation_count": relation_count,
            "present_relation_count": present_relation_count,
            "total_bytes": total_bytes,
            "table_bytes": table_bytes,
            "index_bytes": index_bytes,
            "auxiliary_bytes": auxiliary_bytes,
        },
        "relations": relations,
    }))
}

pub(super) fn retention_expired_total(counts: [i64; 6]) -> i64 {
    counts.into_iter().map(|count| count.max(0)).sum()
}

pub(super) fn retention_cutoff(now: DateTime<Utc>, retention_days: i64) -> DateTime<Utc> {
    now - ChronoDuration::days(retention_days)
}

pub(super) async fn delete_request_traces_before(
    pool: &PgPool,
    cutoff: DateTime<Utc>,
    batch_size: i64,
) -> anyhow::Result<u64> {
    let mut tx = pool.begin().await?;
    if !rotation::try_prune_lock(&mut tx).await? {
        return Ok(0);
    }
    let result = sqlx::query(
        r#"
        WITH doomed AS (
            SELECT id
            FROM request_traces
            WHERE started_at < $1
            ORDER BY started_at ASC, id ASC
            LIMIT $2
        )
        DELETE FROM request_traces r
        USING doomed
        WHERE r.id = doomed.id
        "#,
    )
    .bind(cutoff)
    .bind(batch_size)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(result.rows_affected())
}

pub(super) async fn delete_rollups_before(
    pool: &PgPool,
    cutoff: DateTime<Utc>,
    batch_size: i64,
) -> anyhow::Result<u64> {
    let mut tx = pool.begin().await?;
    let buckets: Vec<DateTime<Utc>> = sqlx::query_scalar(
        r#"SELECT bucket FROM trace_rollups_minute
           WHERE bucket < $1
             AND NOT EXISTS (
                 SELECT 1 FROM request_traces
                 WHERE started_at >= bucket AND started_at < bucket + interval '1 minute'
             )
           ORDER BY bucket LIMIT $2 FOR UPDATE SKIP LOCKED"#,
    )
    .bind(cutoff)
    .bind(batch_size)
    .fetch_all(&mut *tx)
    .await?;
    // Recheck after locking; an inserting writer may have populated a bucket
    // since the first statement's snapshot. Never drop a retained bucket.
    let result = sqlx::query(
        r#"DELETE FROM trace_rollups_minute WHERE bucket = ANY($1)
           AND NOT EXISTS (
               SELECT 1 FROM request_traces
               WHERE started_at >= bucket AND started_at < bucket + interval '1 minute'
           )"#,
    )
    .bind(&buckets)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(result.rows_affected())
}

pub(super) async fn delete_empty_sessions_before(
    pool: &PgPool,
    cutoff: DateTime<Utc>,
    batch_size: i64,
) -> anyhow::Result<u64> {
    let mut tx = pool.begin().await?;
    // Skip sessions currently being updated by the trace writer. Recheck in a
    // fresh statement after locking so a concurrent insert cannot be orphaned.
    let ids: Vec<Uuid> = sqlx::query_scalar(
        r#"SELECT s.id FROM trace_sessions s
           WHERE s.last_seen < $1
             AND NOT EXISTS (SELECT 1 FROM request_traces r WHERE r.session_id = s.id)
           ORDER BY s.last_seen, s.id LIMIT $2 FOR UPDATE OF s SKIP LOCKED"#,
    )
    .bind(cutoff)
    .bind(batch_size)
    .fetch_all(&mut *tx)
    .await?;
    let result = sqlx::query(
        r#"DELETE FROM trace_sessions s WHERE s.id = ANY($1)
           AND NOT EXISTS (SELECT 1 FROM request_traces r WHERE r.session_id = s.id)"#,
    )
    .bind(&ids)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(result.rows_affected())
}

pub(super) async fn delete_ui_audit_events_before(
    pool: &PgPool,
    cutoff: DateTime<Utc>,
    batch_size: i64,
) -> anyhow::Result<u64> {
    let result = sqlx::query(
        r#"
        WITH doomed AS (
            SELECT id
            FROM ui_audit_events
            WHERE created_at < $1
            ORDER BY created_at ASC, id ASC
            LIMIT $2
        )
        DELETE FROM ui_audit_events e
        USING doomed
        WHERE e.id = doomed.id
        "#,
    )
    .bind(cutoff)
    .bind(batch_size)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

pub(super) async fn delete_expired_ui_sessions(
    pool: &PgPool,
    now: DateTime<Utc>,
    batch_size: i64,
) -> anyhow::Result<u64> {
    let result = sqlx::query(
        r#"
        WITH doomed AS (
            SELECT id
            FROM ui_sessions
            WHERE expires_at < $1
            ORDER BY expires_at ASC
            LIMIT $2
        )
        DELETE FROM ui_sessions s
        USING doomed
        WHERE s.id = doomed.id
        "#,
    )
    .bind(now)
    .bind(batch_size)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

pub(super) async fn delete_expired_oauth_states(
    pool: &PgPool,
    now: DateTime<Utc>,
    batch_size: i64,
) -> anyhow::Result<u64> {
    let result = sqlx::query(
        r#"
        WITH doomed AS (
            SELECT state
            FROM oauth_states
            WHERE expires_at < $1
            ORDER BY expires_at ASC
            LIMIT $2
        )
        DELETE FROM oauth_states s
        USING doomed
        WHERE s.state = doomed.state
        "#,
    )
    .bind(now)
    .bind(batch_size)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}
