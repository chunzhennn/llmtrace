//! Background size retention and durable, post-commit archive file collection.
use super::*;
use std::time::Instant;

#[cfg(test)]
mod tests;

const MAINTENANCE_TIME_BUDGET: Duration = Duration::from_secs(30);

pub fn spawn_archive_maintenance(
    pool: PgPool,
    config: StorageConfig,
    archive: ArchiveConfig,
    metrics: RuntimeMetrics,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match maintain_archive(&pool, &config, &archive).await {
                Ok(deleted) if deleted > 0 => {
                    metrics.retention_succeeded(&RetentionPruneResult {
                        request_traces: deleted,
                        ..Default::default()
                    });
                    tracing::info!(request_traces = deleted, "archive size rotation completed");
                }
                Ok(_) => {}
                Err(error) => {
                    metrics.retention_failed(error.to_string());
                    tracing::warn!(%error, "archive maintenance failed; will retry");
                }
            }
            tokio::time::sleep(Duration::from_secs(config.rotate_check_interval_secs)).await;
        }
    })
}

// Use a two-integer namespace distinct from the per-session bigint locks.
pub(super) async fn try_prune_lock(tx: &mut Transaction<'_, Postgres>) -> anyhow::Result<bool> {
    Ok(
        sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(1819045236, 1)")
            .fetch_one(&mut **tx)
            .await?,
    )
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct ArchiveFootprint {
    pub retained_bytes: i64,
    pub pending_bytes: i64,
    pub pending_files: i64,
}

pub(super) async fn archive_footprint(
    tx: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<ArchiveFootprint> {
    Ok(sqlx::query_as(
        r#"SELECT
            (COALESCE((SELECT SUM(compressed_bytes) FROM payload_archive_segments), 0)
             + COALESCE((SELECT SUM(octet_length(request_body_compressed)::bigint
                                    + octet_length(response_body_compressed)::bigint)
                         FROM request_traces
                         WHERE octet_length(request_body_compressed) > 0
                            OR octet_length(response_body_compressed) > 0), 0))::bigint AS retained_bytes,
            COALESCE(SUM(compressed_bytes), 0)::bigint AS pending_bytes,
            COUNT(*)::bigint AS pending_files
           FROM archive_file_deletions"#,
    )
    .fetch_one(&mut **tx)
    .await?)
}

pub(super) async fn maintain_archive(
    pool: &PgPool,
    config: &StorageConfig,
    archive: &ArchiveConfig,
) -> anyhow::Result<u64> {
    let deadline = Instant::now() + MAINTENANCE_TIME_BUDGET;
    let mut deleted = 0;
    loop {
        // Always finish already committed deletions, including after rotation
        // is disabled. If storage cannot be reclaimed, do not evict more data.
        if !collect_archive_files(pool, archive, deadline).await? || Instant::now() >= deadline {
            break;
        }
        if config.rotate_size_bytes == 0 {
            break;
        }
        let removed = rotate_batch(pool, config).await?;
        deleted += removed;
        // Session cleanup runs after the rollup locks have been released.
        delete_empty_sessions_before(pool, Utc::now(), config.retention_prune_batch_size).await?;
        if removed == 0 {
            collect_archive_files(pool, archive, deadline).await?;
            break;
        }
    }
    Ok(deleted)
}

async fn rotate_batch(pool: &PgPool, config: &StorageConfig) -> anyhow::Result<u64> {
    let deadline = Instant::now() + MAINTENANCE_TIME_BUDGET;
    let mut tx = pool.begin().await?;
    if !try_prune_lock(&mut tx).await? {
        return Ok(0);
    }
    sqlx::query("SET LOCAL statement_timeout = '30s'")
        .execute(&mut *tx)
        .await?;
    // Reclaim indexed orphans left by older versions before removing requests.
    sqlx::query(
        r#"DELETE FROM payload_archive_segments WHERE id IN (
            SELECT s.id FROM payload_archive_segments s
            WHERE NOT EXISTS (SELECT 1 FROM payload_archive_records r WHERE r.segment_id = s.id)
            ORDER BY s.created_at, s.id LIMIT $1
        )"#,
    )
    .bind(config.retention_prune_batch_size)
    .execute(&mut *tx)
    .await?;
    let footprint = archive_footprint(&mut tx).await?;
    // Commit and collect orphan files before making further eviction decisions.
    if footprint.pending_files > 0 {
        tx.commit().await?;
        return Ok(0);
    }
    let mut size = footprint.retained_bytes;
    let limit = i64::try_from(config.rotate_size_bytes)?;
    let mut deleted = 0;
    if size > limit {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM request_traces ORDER BY started_at, id LIMIT $1 FOR UPDATE",
        )
        .bind(config.retention_prune_batch_size)
        .fetch_all(&mut *tx)
        .await?;
        for id in ids {
            // Shared legacy segments only release space at their last request.
            let freed: i64 = sqlx::query_scalar(
                r#"SELECT (COALESCE((SELECT SUM(s.compressed_bytes)
                    FROM payload_archive_segments s
                    WHERE EXISTS (SELECT 1 FROM payload_archive_records r WHERE r.segment_id = s.id AND r.trace_id = $1)
                      AND NOT EXISTS (SELECT 1 FROM payload_archive_records r WHERE r.segment_id = s.id AND r.trace_id <> $1)), 0)
                    + (SELECT octet_length(request_body_compressed)::bigint
                              + octet_length(response_body_compressed)::bigint
                       FROM request_traces WHERE id = $1))::bigint"#,
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
            deleted += sqlx::query("DELETE FROM request_traces WHERE id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?
                .rows_affected();
            size = size.saturating_sub(freed);
            if size <= limit || Instant::now() >= deadline {
                break;
            }
        }
    }
    tx.commit().await?;
    Ok(deleted)
}

async fn collect_archive_files(
    pool: &PgPool,
    archive: &ArchiveConfig,
    deadline: Instant,
) -> anyhow::Result<bool> {
    while Instant::now() < deadline {
        let mut tx = pool.begin().await?;
        let key: Option<String> = sqlx::query_scalar(
            "SELECT storage_key FROM archive_file_deletions ORDER BY queued_at, storage_key LIMIT 1 FOR UPDATE SKIP LOCKED",
        ).fetch_optional(&mut *tx).await?;
        let Some(key) = key else {
            // Another instance may be deleting the remaining files. Wait for a
            // subsequent pass before evicting additional requests.
            let empty: bool =
                sqlx::query_scalar("SELECT NOT EXISTS (SELECT 1 FROM archive_file_deletions)")
                    .fetch_one(&mut *tx)
                    .await?;
            tx.commit().await?;
            return Ok(empty);
        };
        let path = archive_file_path(&archive.filesystem_root, &key)?;
        tokio::task::spawn_blocking(move || match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("failed to remove retired archive file"),
        })
        .await??;
        sqlx::query("DELETE FROM archive_file_deletions WHERE storage_key = $1")
            .bind(&key)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    Ok(false)
}
