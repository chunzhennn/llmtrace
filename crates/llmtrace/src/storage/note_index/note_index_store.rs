//! Durable prompt tag jobs, typed row validation, fencing and atomic publication.
use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::config::ArchiveConfig;
use crate::prompt_tags::{ExtractionState, NoteDiagnostic, NoteIndexVersion, TagExtraction};
use crate::storage::bodies;
use crate::storage::message_contents::ContentHash;
use crate::storage::models::{BodyStatus, REQUEST_METADATA_COLUMNS, RequestMetadata};

#[derive(Debug, thiserror::Error)]
pub(super) enum IndexStoreError {
    #[error("prompt tag index database operation failed")]
    Database(#[from] sqlx::Error),
    #[error("prompt tag index source read failed")]
    Source(#[source] anyhow::Error),
    #[error("invalid persisted prompt tag job state")]
    InvalidState,
    #[error("unsupported persisted prompt tag index version")]
    UnknownVersion,
    #[error("prompt tag value dictionary integrity mismatch")]
    ContentMismatch,
}

struct RequestId(Uuid);
struct LeaseToken(Uuid);

enum IndexJobState {
    Pending,
    Processing {
        token: LeaseToken,
        deadline: DateTime<Utc>,
    },
    Terminal,
}

fn decode_state(
    state: &str,
    token: Option<Uuid>,
    deadline: Option<DateTime<Utc>>,
) -> Result<IndexJobState, IndexStoreError> {
    match (state, token, deadline) {
        ("pending", None, None) => Ok(IndexJobState::Pending),
        ("processing", Some(token), Some(deadline)) if !token.is_nil() => {
            Ok(IndexJobState::Processing {
                token: LeaseToken(token),
                deadline,
            })
        }
        ("ready" | "partial" | "unsupported" | "failed", None, None) => Ok(IndexJobState::Terminal),
        _ => Err(IndexStoreError::InvalidState),
    }
}

/// Non-cloneable claim. Dropping it leaves the lease for durable recovery; Drop
/// never performs async SQL or starts a detached cleanup task.
#[must_use]
pub(super) struct ClaimedNoteJob {
    pub(super) tags: Vec<String>,
    request_id: RequestId,
    version: NoteIndexVersion,
    token: LeaseToken,
    deadline: DateTime<Utc>,
}

impl ClaimedNoteJob {
    pub(super) fn request_id(&self) -> Uuid {
        self.request_id.0
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum PublishOutcome {
    Published,
    LeaseLost,
    SourceRemoved,
}

pub(super) struct LoadedPrompt {
    pub(super) metadata: RequestMetadata,
    pub(super) body: Option<Vec<u8>>,
    pub(super) unavailable: Option<TagExtraction>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum PromptIndexStatus {
    Disabled,
    NotIndexed,
    Pending,
    Processing,
    Ready,
    Partial,
    Unsupported,
    Failed,
}

#[derive(Serialize)]
pub(crate) struct RequestIndexStatus {
    index_version: NoteIndexVersion,
    state: PromptIndexStatus,
    capture_complete: bool,
    external_history: bool,
    diagnostics: Vec<NoteDiagnostic>,
    indexed_at: Option<DateTime<Utc>>,
    attempt_count: i32,
}

/// Metadata-only status. Lease tokens and raw prompt text are never exposed.
pub(crate) async fn request_index_status(
    pool: &PgPool,
    id: Uuid,
) -> anyhow::Result<Option<RequestIndexStatus>> {
    let mut tx = crate::storage::begin_api_read_tx(pool).await?;
    let row = sqlx::query("SELECT settings.active_version, cardinality(settings.tag_names) = 0 AS disabled, i.state, COALESCE(i.capture_complete, NOT r.request_body_truncated) AS capture_complete,
        i.external_history, i.diagnostics, i.indexed_at, i.attempt_count, i.lease_token, i.lease_until
        FROM request_traces r CROSS JOIN note_index_settings settings
        LEFT JOIN request_prompt_index_state i ON i.request_id = r.id AND i.index_version = settings.active_version
            AND i.tag_revision = settings.tag_revision AND cardinality(settings.tag_names) > 0
        WHERE r.id = $1 AND settings.singleton")
        .bind(id).fetch_optional(&mut *tx).await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let version = NoteIndexVersion::try_from(row.try_get::<i32, _>("active_version")?)
        .map_err(anyhow::Error::msg)?;
    let state: Option<&str> = row.try_get("state")?;
    let status = if row.try_get::<bool, _>("disabled")? {
        PromptIndexStatus::Disabled
    } else if let Some(state) = state {
        decode_state(
            state,
            row.try_get("lease_token")?,
            row.try_get("lease_until")?,
        )?;
        match state {
            "pending" => PromptIndexStatus::Pending,
            "processing" => PromptIndexStatus::Processing,
            "ready" => PromptIndexStatus::Ready,
            "partial" => PromptIndexStatus::Partial,
            "unsupported" => PromptIndexStatus::Unsupported,
            "failed" => PromptIndexStatus::Failed,
            _ => return Err(IndexStoreError::InvalidState.into()),
        }
    } else {
        PromptIndexStatus::NotIndexed
    };
    let diagnostics: Option<sqlx::types::Json<Vec<NoteDiagnostic>>> = row.try_get("diagnostics")?;
    let result = RequestIndexStatus {
        index_version: version,
        state: status,
        capture_complete: row.try_get("capture_complete")?,
        external_history: row
            .try_get::<Option<bool>, _>("external_history")?
            .unwrap_or(false),
        diagnostics: diagnostics.map(|value| value.0).unwrap_or_default(),
        indexed_at: row.try_get("indexed_at")?,
        attempt_count: row.try_get::<Option<i32>, _>("attempt_count")?.unwrap_or(0),
    };
    tx.commit().await?;
    Ok(Some(result))
}

pub(super) async fn worker_settings(
    pool: &PgPool,
) -> Result<(NoteIndexVersion, bool, bool), IndexStoreError> {
    let (version, tags_enabled, backfill): (i32, bool, bool) = sqlx::query_as(
        "SELECT active_version, cardinality(tag_names) > 0, backfill_enabled FROM note_index_settings WHERE singleton",
    ).fetch_one(pool).await?;
    let version =
        NoteIndexVersion::try_from(version).map_err(|_| IndexStoreError::UnknownVersion)?;
    Ok((version, tags_enabled, backfill))
}

pub(super) async fn refresh_coverage(
    pool: &PgPool,
    version: NoteIndexVersion,
) -> Result<(), IndexStoreError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL statement_timeout = '2s'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO note_index_coverage(index_version, tag_revision, checked_at, pending, processing, ready, partial, unsupported, failed, missing_state, missing_source)
        SELECT $1, cfg.tag_revision, now(), COUNT(*) FILTER (WHERE i.state = 'pending'), COUNT(*) FILTER (WHERE i.state = 'processing'),
            COUNT(*) FILTER (WHERE i.state = 'ready'), COUNT(*) FILTER (WHERE i.state = 'partial'),
            COUNT(*) FILTER (WHERE i.state = 'unsupported'), COUNT(*) FILTER (WHERE i.state = 'failed'), COUNT(*) FILTER (WHERE r.id IS NOT NULL AND i.request_id IS NULL), COUNT(*) FILTER (WHERE i.diagnostics @> '[\"missing_source\"]'::jsonb)
        FROM note_index_settings cfg LEFT JOIN request_traces r ON cardinality(cfg.tag_names) > 0 LEFT JOIN request_prompt_index_state i
          ON i.request_id = r.id AND i.index_version = $1 AND i.tag_revision = cfg.tag_revision
        WHERE cfg.singleton GROUP BY cfg.tag_revision
        ON CONFLICT (index_version) DO UPDATE SET tag_revision = EXCLUDED.tag_revision, checked_at = EXCLUDED.checked_at, pending = EXCLUDED.pending,
            processing = EXCLUDED.processing, ready = EXCLUDED.ready, partial = EXCLUDED.partial,
            unsupported = EXCLUDED.unsupported, failed = EXCLUDED.failed, missing_state = EXCLUDED.missing_state, missing_source = EXCLUDED.missing_source")
        .bind(i32::from(version)).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Idempotent bounded backfill; rescanning missing state catches old writers and
/// transactions committed behind a previous scan. Retention cannot be revived.
pub(super) async fn enqueue_missing(
    pool: &PgPool,
    version: NoteIndexVersion,
    limit: i64,
) -> Result<u64, IndexStoreError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL statement_timeout = '2s'")
        .execute(&mut *tx)
        .await?;
    // Cascading deletion also drops old positions and invalidates their leases.
    sqlx::query("DELETE FROM request_prompt_index_state WHERE (request_id, index_version) IN (
        SELECT request_id, index_version FROM request_prompt_index_state
        WHERE index_version = $1 AND tag_revision < (SELECT tag_revision FROM note_index_settings WHERE singleton)
        LIMIT $2 FOR UPDATE SKIP LOCKED)")
        .bind(i32::from(version)).bind(limit).execute(&mut *tx).await?;
    let count = sqlx::query("INSERT INTO request_prompt_index_state(request_id, index_version, capture_complete, priority, tag_revision)
        SELECT r.id, $1, NOT r.request_body_truncated, 1, cfg.tag_revision FROM request_traces r CROSS JOIN note_index_settings cfg
        WHERE cfg.singleton AND cardinality(cfg.tag_names) > 0
          AND NOT EXISTS (SELECT 1 FROM request_prompt_index_state i WHERE i.request_id = r.id AND i.index_version = $1)
        ORDER BY r.started_at DESC, r.id LIMIT $2 ON CONFLICT DO NOTHING")
        .bind(i32::from(version)).bind(limit).execute(&mut *tx).await?.rows_affected();
    tx.commit().await?;
    Ok(count)
}

/// Uses database time, not local clocks. No row lock survives this short claim.
pub(super) async fn claim(
    pool: &PgPool,
    version: NoteIndexVersion,
    lease_seconds: i64,
    max_attempts: i32,
) -> Result<Option<ClaimedNoteJob>, IndexStoreError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL statement_timeout = '2s'")
        .execute(&mut *tx)
        .await?;
    let row = sqlx::query("WITH exhausted AS (
        UPDATE request_prompt_index_state SET state = 'failed', diagnostics = '[\"retry_exhausted\"]', lease_token = NULL, lease_until = NULL
        WHERE (request_id, index_version) IN (
            SELECT request_id, index_version FROM request_prompt_index_state
            WHERE index_version = $1 AND attempt_count >= $2
              AND ((state = 'pending' AND next_retry_at <= now()) OR (state = 'processing' AND lease_until <= now()))
            ORDER BY priority, next_retry_at, request_id LIMIT 100 FOR UPDATE SKIP LOCKED
        )
    ) SELECT request_id, index_version, state, lease_token, lease_until, cfg.tag_names, clock_timestamp() AS claimed_at
        FROM request_prompt_index_state i JOIN note_index_settings cfg ON cfg.singleton AND cfg.tag_revision = i.tag_revision AND cardinality(cfg.tag_names) > 0
        WHERE index_version = $1 AND attempt_count < $2
          AND ((state = 'pending' AND next_retry_at <= now()) OR (state = 'processing' AND lease_until <= now()))
        ORDER BY priority, next_retry_at, request_id LIMIT 1 FOR UPDATE OF i SKIP LOCKED")
        .bind(i32::from(version)).bind(max_attempts).fetch_optional(&mut *tx).await?;
    let Some(row) = row else {
        tx.commit().await?;
        return Ok(None);
    };
    let state = decode_state(
        row.try_get("state")?,
        row.try_get("lease_token")?,
        row.try_get("lease_until")?,
    )?;
    match state {
        IndexJobState::Pending => {}
        IndexJobState::Processing { token, deadline } => {
            if token.0.is_nil() || deadline > row.try_get::<DateTime<Utc>, _>("claimed_at")? {
                return Err(IndexStoreError::InvalidState);
            }
        }
        IndexJobState::Terminal => return Err(IndexStoreError::InvalidState),
    }
    let request_id = RequestId(row.try_get("request_id")?);
    let version = NoteIndexVersion::try_from(row.try_get::<i32, _>("index_version")?)
        .map_err(|_| IndexStoreError::UnknownVersion)?;
    let token = LeaseToken(Uuid::new_v4());
    let deadline = sqlx::query_scalar("UPDATE request_prompt_index_state SET state = 'processing', lease_token = $3,
        lease_until = now() + make_interval(secs => $4::double precision), attempt_count = attempt_count + 1
        WHERE request_id = $1 AND index_version = $2 RETURNING lease_until")
        .bind(request_id.0).bind(i32::from(version)).bind(token.0).bind(lease_seconds as f64).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(Some(ClaimedNoteJob {
        tags: row.try_get("tag_names")?,
        request_id,
        version,
        token,
        deadline,
    }))
}

pub(super) async fn load_source(
    pool: &PgPool,
    archive: &ArchiveConfig,
    job: &ClaimedNoteJob,
    max_bytes: usize,
    max_segment_bytes: usize,
) -> Result<Option<LoadedPrompt>, IndexStoreError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION READ ONLY")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL statement_timeout = '5s'")
        .execute(&mut *tx)
        .await?;
    let query = format!("SELECT {REQUEST_METADATA_COLUMNS} FROM request_traces WHERE id = $1");
    let metadata = sqlx::query_as::<_, RequestMetadata>(&query)
        .bind(job.request_id.0)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(mut metadata) = metadata else {
        tx.commit().await?;
        return Ok(None);
    };
    let complete = !metadata.summary.request_body_truncated;
    let observed =
        usize::try_from(metadata.request_body_bytes).map_err(|_| IndexStoreError::InvalidState)?;
    let (body, unavailable) = if observed > max_bytes {
        let mut result = TagExtraction::unavailable(NoteDiagnostic::BodyBudget, complete);
        result.state = ExtractionState::Partial;
        (None, Some(result))
    } else {
        match bodies::load_request_body(&mut tx, archive, &metadata, max_segment_bytes).await {
            Err(error) if error.is::<bodies::BodyReadBudgetExceeded>() => {
                let mut result = TagExtraction::unavailable(NoteDiagnostic::BodyBudget, complete);
                result.state = ExtractionState::Partial;
                (None, Some(result))
            }
            Err(error) => return Err(IndexStoreError::Source(error)),
            Ok(body) => {
                metadata.summary.request_body_truncated |= body.truncated;
                match body.status {
                    BodyStatus::Available => (body.bytes, None),
                    BodyStatus::Missing => (
                        None,
                        Some(TagExtraction::unavailable(
                            NoteDiagnostic::MissingSource,
                            complete,
                        )),
                    ),
                    BodyStatus::Unreadable => (
                        None,
                        Some(TagExtraction::unavailable(
                            NoteDiagnostic::UnreadableSource,
                            complete,
                        )),
                    ),
                }
            }
        }
    };
    tx.commit().await?;
    Ok(Some(LoadedPrompt {
        metadata,
        body,
        unavailable,
    }))
}

/// Consumes the claim and checks token AND deadline before touching old results.
pub(super) async fn publish(
    pool: &PgPool,
    job: ClaimedNoteJob,
    result: TagExtraction,
) -> Result<PublishOutcome, IndexStoreError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL statement_timeout = '5s'")
        .execute(&mut *tx)
        .await?;
    // Same request -> state lock order as retention's FK cascade.
    let source: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM request_traces WHERE id = $1 FOR KEY SHARE")
            .bind(job.request_id.0)
            .fetch_optional(&mut *tx)
            .await?;
    if source.is_none() {
        return Ok(PublishOutcome::SourceRemoved);
    }
    let valid: Option<bool> = sqlx::query_scalar("SELECT state = 'processing' AND lease_token = $3 AND lease_until = $4 AND lease_until > clock_timestamp()
        FROM request_prompt_index_state i JOIN note_index_settings cfg ON cfg.singleton AND i.tag_revision = cfg.tag_revision
        WHERE request_id = $1 AND index_version = $2 FOR UPDATE OF i")
        .bind(job.request_id.0).bind(i32::from(job.version)).bind(job.token.0).bind(job.deadline).fetch_optional(&mut *tx).await?;
    if valid != Some(true) {
        return Ok(PublishOutcome::LeaseLost);
    }
    let values = intern_values(&mut tx, job.version, &result).await?;
    // A retry never erases valid same-version positive evidence on failure.
    if result.state == ExtractionState::Ready {
        sqlx::query("DELETE FROM request_notes WHERE request_id = $1 AND index_version = $2")
            .bind(job.request_id.0)
            .bind(i32::from(job.version))
            .execute(&mut *tx)
            .await?;
    }
    if !result.notes.is_empty() {
        let positioned = result
            .notes
            .iter()
            .map(|note| {
                let id = values
                    .get(&ContentHash::of(&note.value))
                    .copied()
                    .ok_or(IndexStoreError::ContentMismatch)?;
                let start =
                    i32::try_from(note.span.start()).map_err(|_| IndexStoreError::InvalidState)?;
                let end =
                    i32::try_from(note.span.end()).map_err(|_| IndexStoreError::InvalidState)?;
                Ok((note, id, start, end))
            })
            .collect::<Result<Vec<_>, IndexStoreError>>()?;
        let mut builder = sqlx::QueryBuilder::<Postgres>::new(
            "INSERT INTO request_notes(id, request_id, index_version, field_ordinal, match_ordinal, source_path, source_kind, role, note_value_id, decoded_start_byte, decoded_end_byte, tag_name) ",
        );
        builder.push_values(&positioned, |mut row, &(note, id, start, end)| {
            row.push_bind(Uuid::new_v4())
                .push_bind(job.request_id.0)
                .push_bind(i32::from(job.version))
                .push_bind(note.field_ordinal)
                .push_bind(note.match_ordinal)
                .push_bind(&note.source_path)
                .push_bind(note.source_kind.as_str())
                .push_bind(&note.role)
                .push_bind(id)
                .push_bind(start)
                .push_bind(end)
                .push_bind(&note.tag_name);
        });
        builder.push(
            " ON CONFLICT (request_id, index_version, field_ordinal, match_ordinal) DO NOTHING",
        );
        builder.build().execute(&mut *tx).await?;
    }
    sqlx::query("UPDATE request_prompt_index_state SET state = $3, capture_complete = $4, external_history = $5,
        diagnostics = $6, indexed_at = now(), lease_token = NULL, lease_until = NULL WHERE request_id = $1 AND index_version = $2")
        .bind(job.request_id.0).bind(i32::from(job.version)).bind(result.state.as_str()).bind(result.capture_complete)
        .bind(result.external_history).bind(sqlx::types::Json(&result.diagnostics)).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(PublishOutcome::Published)
}

async fn intern_values(
    tx: &mut Transaction<'_, Postgres>,
    version: NoteIndexVersion,
    result: &TagExtraction,
) -> Result<BTreeMap<ContentHash, i64>, IndexStoreError> {
    let mut unique = BTreeMap::new();
    for note in &result.notes {
        let hash = ContentHash::of(&note.value);
        if unique
            .insert(hash, note.value.as_str())
            .is_some_and(|old| old != note.value)
        {
            return Err(IndexStoreError::ContentMismatch);
        }
    }
    if unique.is_empty() {
        return Ok(BTreeMap::new());
    }
    let hashes: Vec<Vec<u8>> = unique.keys().map(|hash| hash.0.to_vec()).collect();
    let values: Vec<&str> = unique.values().copied().collect();
    for _ in 0..3 {
        sqlx::query("INSERT INTO note_values(index_version, value_sha256, value, value_bytes)
            SELECT $1, hash, value, octet_length(value) FROM UNNEST($2::bytea[], $3::text[]) AS input(hash, value)
            ORDER BY hash ON CONFLICT (index_version, value_sha256) DO NOTHING")
            .bind(i32::from(version)).bind(&hashes).bind(&values).execute(&mut **tx).await?;
        let rows = sqlx::query("SELECT id, value_sha256, value, value_bytes FROM note_values WHERE index_version = $1 AND value_sha256 = ANY($2) ORDER BY id FOR KEY SHARE")
            .bind(i32::from(version)).bind(&hashes).fetch_all(&mut **tx).await?;
        if rows.len() != unique.len() {
            continue;
        }
        let mut ids = BTreeMap::new();
        for row in rows {
            let bytes: Vec<u8> = row.try_get("value_sha256")?;
            let hash = ContentHash(
                bytes
                    .try_into()
                    .map_err(|_| IndexStoreError::ContentMismatch)?,
            );
            let value: &str = row.try_get("value")?;
            if unique.get(&hash).copied() != Some(value)
                || usize::try_from(row.try_get::<i32, _>("value_bytes")?).ok() != Some(value.len())
            {
                return Err(IndexStoreError::ContentMismatch);
            }
            ids.insert(hash, row.try_get("id")?);
        }
        if result
            .notes
            .iter()
            .any(|note| !ids.contains_key(&ContentHash::of(&note.value)))
        {
            return Err(IndexStoreError::ContentMismatch);
        }
        return Ok(ids);
    }
    Err(IndexStoreError::ContentMismatch)
}

/// Limited retry only for I/O/store failures. Never clears published prompt tag rows.
pub(super) async fn retry(
    pool: &PgPool,
    job: ClaimedNoteJob,
    max_attempts: i32,
) -> Result<(), IndexStoreError> {
    sqlx::query("UPDATE request_prompt_index_state SET state = CASE WHEN attempt_count >= $4 THEN 'failed' ELSE 'pending' END,
        diagnostics = CASE WHEN attempt_count >= $4 THEN '[\"retry_exhausted\"]'::jsonb ELSE diagnostics END,
        next_retry_at = now() + interval '10 seconds', lease_token = NULL, lease_until = NULL
        WHERE request_id = $1 AND index_version = $2 AND state = 'processing' AND lease_token = $3")
        .bind(job.request_id.0).bind(i32::from(job.version)).bind(job.token.0).bind(max_attempts).execute(pool).await?;
    Ok(())
}

/// Fixed prompt tag dictionary GC follows the preview dictionary's reference-lock
/// protocol. No generic table-name SQL, mutable refcount or independent service.
pub(super) async fn collect_unused(pool: &PgPool, limit: i64) -> Result<u64, IndexStoreError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL statement_timeout = '5s'")
        .execute(&mut *tx)
        .await?;
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM note_values v WHERE NOT EXISTS (SELECT 1 FROM request_notes n WHERE n.note_value_id = v.id) ORDER BY id LIMIT $1 FOR UPDATE SKIP LOCKED")
        .bind(limit).fetch_all(&mut *tx).await?;
    let count = sqlx::query("DELETE FROM note_values v WHERE id = ANY($1) AND NOT EXISTS (SELECT 1 FROM request_notes n WHERE n.note_value_id = v.id)")
        .bind(&ids).execute(&mut *tx).await?.rows_affected();
    tx.commit().await?;
    Ok(count)
}

#[cfg(test)]
mod tests;
