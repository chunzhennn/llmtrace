//! Explicit, bounded operator actions for derived prompt tag indexes and versions.
use serde::Serialize;
use sqlx::PgPool;

use crate::prompt_tags::NoteIndexVersion;

use super::note_index_store;

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub(crate) enum NoteMaintenanceAction {
    Status,
    PauseBackfill,
    ResumeBackfill,
    Backfill,
    RetryFailed,
    Activate,
    PrepareSubstring,
}

#[derive(Serialize)]
struct MaintenanceResult {
    index_version: NoteIndexVersion,
    affected_rows: u64,
}

/// Requires explicit CLI configuration. No production URL fallback or unbounded
/// replay loop. Activating a supported version checks its retained-request state.
pub(crate) async fn maintain(
    pool: &PgPool,
    action: NoteMaintenanceAction,
    version: NoteIndexVersion,
    batch_size: i64,
) -> anyhow::Result<serde_json::Value> {
    anyhow::ensure!(
        (1..=1000).contains(&batch_size),
        "prompt tag maintenance batch must be 1..=1000"
    );
    use NoteMaintenanceAction as Action;
    if matches!(action, Action::PrepareSubstring) {
        // These static DDL statements intentionally run outside a transaction.
        sqlx::query("CREATE EXTENSION IF NOT EXISTS pg_trgm")
            .execute(pool)
            .await?;
        let valid: Option<bool> = sqlx::query_scalar("SELECT indisvalid FROM pg_index WHERE indexrelid = to_regclass('idx_note_values_trgm')").fetch_optional(pool).await?;
        if valid == Some(false) {
            sqlx::query("DROP INDEX CONCURRENTLY idx_note_values_trgm")
                .execute(pool)
                .await?;
        }
        sqlx::query("CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_note_values_trgm ON note_values USING GIN (value gin_trgm_ops)").execute(pool).await?;
        return Ok(serde_json::json!({"substring_index_ready":true}));
    }
    if matches!(action, Action::Backfill) {
        let affected_rows = note_index_store::enqueue_missing(pool, version, batch_size).await?;
        return Ok(serde_json::to_value(MaintenanceResult {
            index_version: version,
            affected_rows,
        })?);
    }
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL statement_timeout = '5s'")
        .execute(&mut *tx)
        .await?;
    let mut affected_rows = 0;
    match action {
        Action::PauseBackfill | Action::ResumeBackfill => {
            affected_rows =
                sqlx::query("UPDATE note_index_settings SET backfill_enabled = $1 WHERE singleton")
                    .bind(matches!(action, Action::ResumeBackfill))
                    .execute(&mut *tx)
                    .await?
                    .rows_affected();
        }
        Action::RetryFailed => {
            affected_rows = sqlx::query("UPDATE request_prompt_index_state SET state = 'pending', attempt_count = 0, next_retry_at = now(), lease_token = NULL, lease_until = NULL
                WHERE (request_id, index_version) IN (SELECT request_id, index_version FROM request_prompt_index_state WHERE index_version = $1 AND state = 'failed' ORDER BY request_id LIMIT $2 FOR UPDATE SKIP LOCKED)")
                .bind(i32::from(version)).bind(batch_size).execute(&mut *tx).await?.rows_affected();
        }
        Action::Activate => {
            // Serialize activation with upgraded ingest's FOR SHARE setting read
            // so a committed trace cannot miss the newly active version.
            sqlx::query("SELECT singleton FROM note_index_settings WHERE singleton FOR UPDATE")
                .execute(&mut *tx)
                .await?;
            let remaining: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM request_traces r CROSS JOIN note_index_settings cfg LEFT JOIN request_prompt_index_state i ON i.request_id = r.id AND i.index_version = $1 AND i.tag_revision = cfg.tag_revision WHERE cfg.singleton AND cardinality(cfg.tag_names) > 0 AND (i.request_id IS NULL OR i.state IN ('pending','processing','failed')))")
                .bind(i32::from(version)).fetch_one(&mut *tx).await?;
            anyhow::ensure!(
                !remaining,
                "finish supported-version backfill and resolve pending/failed requests before activation"
            );
            affected_rows =
                sqlx::query("UPDATE note_index_settings SET active_version = $1 WHERE singleton")
                    .bind(i32::from(version))
                    .execute(&mut *tx)
                    .await?
                    .rows_affected();
        }
        Action::Status | Action::Backfill | Action::PrepareSubstring => {}
    }
    tx.commit().await?;
    if matches!(action, Action::Status) {
        note_index_store::refresh_coverage(pool, version).await?;
        let summary: serde_json::Value = sqlx::query_scalar(
            "SELECT to_jsonb(c) FROM note_index_coverage c WHERE index_version = $1",
        )
        .bind(i32::from(version))
        .fetch_one(pool)
        .await?;
        return Ok(summary);
    }
    Ok(serde_json::to_value(MaintenanceResult {
        index_version: version,
        affected_rows,
    })?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt_tags::{NoteDiagnostic, TagExtraction};
    use uuid::Uuid;

    #[sqlx::test(migrations = "./migrations")]
    #[ignore = "requires local PostgreSQL"]
    async fn controls_bound_backfill_retry_and_version_activation(
        pool: PgPool,
    ) -> anyhow::Result<()> {
        use NoteMaintenanceAction as Action;
        let version = NoteIndexVersion::V1;
        crate::storage::note_index::configure_test_tags(&pool, &["Test"]).await?;
        sqlx::query("INSERT INTO request_traces(id, started_at, method, original_uri, upstream_url) VALUES ($1, now(), 'POST', '/', 'https://synthetic.invalid')").bind(Uuid::new_v4()).execute(&pool).await?;
        for (action, enabled) in [
            (Action::PauseBackfill, false),
            (Action::ResumeBackfill, true),
        ] {
            maintain(&pool, action, version, 1).await?;
            let (_, _, backfill) = note_index_store::worker_settings(&pool).await?;
            assert_eq!(backfill, enabled);
        }
        assert_eq!(
            maintain(&pool, Action::Backfill, version, 1).await?["affected_rows"],
            1
        );
        assert!(maintain(&pool, Action::Activate, version, 1).await.is_err());
        let job = note_index_store::claim(&pool, version, 60, 3)
            .await?
            .unwrap();
        note_index_store::publish(
            &pool,
            job,
            TagExtraction::unavailable(NoteDiagnostic::MissingSource, true),
        )
        .await?;
        assert_eq!(
            maintain(&pool, Action::Status, version, 1).await?["missing_source"],
            1
        );
        assert_eq!(
            maintain(&pool, Action::RetryFailed, version, 1).await?["affected_rows"],
            1
        );
        Ok(())
    }

    #[sqlx::test(migrations = "./migrations")]
    #[ignore = "requires PostgreSQL with pg_trgm available"]
    async fn substring_preparation_is_nontransactional_idempotent_and_validated(
        pool: PgPool,
    ) -> anyhow::Result<()> {
        for _ in 0..2 {
            maintain(
                &pool,
                NoteMaintenanceAction::PrepareSubstring,
                NoteIndexVersion::V1,
                1,
            )
            .await?;
        }
        let valid: bool = sqlx::query_scalar(
            "SELECT indisvalid FROM pg_index WHERE indexrelid = 'idx_note_values_trgm'::regclass",
        )
        .fetch_one(&pool)
        .await?;
        assert!(valid);
        Ok(())
    }
}
