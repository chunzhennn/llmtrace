//! Transactional trace ingestion, sessions, rollups and journal receipts.
use super::*;

#[derive(Debug, Clone)]
pub struct TraceRecord {
    pub id: Uuid,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub method: String,
    pub original_uri: String,
    pub upstream_url: String,
    pub upstream_host: Option<String>,
    pub status: Option<i32>,
    pub error: Option<String>,
    pub request_kind: RequestKind,
    pub model: Option<String>,
    pub api_key_hash: Option<String>,
    pub session_key: Option<String>,
    pub session_id: Option<Uuid>,
    pub ttft_ms: Option<i64>,
    pub ttfb_ms: Option<i64>,
    pub usage: crate::types::TokenUsage,
    pub usage_complete: bool,
    pub tool_calls: Vec<crate::types::ToolCall>,
    pub estimated_cost_microusd: Option<i64>,
    pub duration_ms: Option<i64>,
    pub bytes_in: i64,
    pub bytes_out: i64,
    pub request_headers: Value,
    pub response_headers: Value,
    pub request_body: Vec<u8>,
    pub response_body: Vec<u8>,
    pub request_body_bytes: i64,
    pub response_body_bytes: i64,
    pub request_body_truncated: bool,
    pub response_body_truncated: bool,
    pub content_type: Option<String>,
    pub plugin_metadata: Value,
    pub tags: Vec<String>,
}

pub async fn insert_trace(
    pool: &PgPool,
    archive: &ArchiveConfig,
    trace: TraceRecord,
    messages: Vec<ParsedMessage>,
    user_id: Option<String>,
    user_name: Option<String>,
) -> anyhow::Result<()> {
    insert_trace_internal(pool, archive, trace, messages, user_id, user_name, None).await
}

pub(crate) async fn insert_journaled_trace(
    pool: &PgPool,
    archive: &ArchiveConfig,
    trace: TraceRecord,
    messages: Vec<ParsedMessage>,
    user_id: Option<String>,
    user_name: Option<String>,
    journal_id: Uuid,
) -> anyhow::Result<()> {
    insert_trace_internal(
        pool,
        archive,
        trace,
        messages,
        user_id,
        user_name,
        Some(journal_id),
    )
    .await
}

pub(crate) async fn journal_trace_persisted(
    pool: &PgPool,
    trace_id: Uuid,
    journal_id: Uuid,
) -> anyhow::Result<bool> {
    let owner: Option<Uuid> =
        sqlx::query_scalar("SELECT journal_id FROM trace_ingest_receipts WHERE trace_id=$1")
            .bind(trace_id)
            .fetch_optional(pool)
            .await?;
    if let Some(owner) = owner {
        anyhow::ensure!(
            owner == journal_id,
            "trace receipt belongs to a different journal"
        );
    }
    Ok(owner.is_some())
}

pub(super) async fn insert_trace_internal(
    pool: &PgPool,
    archive: &ArchiveConfig,
    mut trace: TraceRecord,
    messages: Vec<ParsedMessage>,
    user_id: Option<String>,
    user_name: Option<String>,
    journal_id: Option<Uuid>,
) -> anyhow::Result<()> {
    // CPU work and large buffer assembly finish before taking a database
    // connection or session/rollup locks. Slow compression cannot serialize
    // other captures of the same conversation.
    let payloads = prepare_trace_payloads(archive, &mut trace).await?;
    let mut tx = pool.begin().await?;
    if let Some(journal_id) = journal_id {
        let inserted = sqlx::query("INSERT INTO trace_ingest_receipts(trace_id,journal_id) VALUES ($1,$2) ON CONFLICT (trace_id) DO NOTHING")
            .bind(trace.id).bind(journal_id).execute(&mut *tx).await?.rows_affected();
        if inserted == 0 {
            let owner: Uuid = sqlx::query_scalar(
                "SELECT journal_id FROM trace_ingest_receipts WHERE trace_id=$1",
            )
            .bind(trace.id)
            .fetch_one(&mut *tx)
            .await?;
            anyhow::ensure!(
                owner == journal_id,
                "trace receipt belongs to a different journal"
            );
            tx.commit().await?;
            return Ok(());
        }
    }

    if trace.session_id.is_none()
        && let Some(session_key) = trace.session_key.clone()
    {
        trace.session_id = Some(
            upsert_session(
                &mut tx,
                &session_key,
                user_id,
                user_name,
                trace.started_at,
                trace.completed_at.unwrap_or(trace.started_at),
            )
            .await?,
        );
    }

    sqlx::query(
        r#"
        INSERT INTO request_traces (
            id, started_at, completed_at, method, original_uri, upstream_url, upstream_host,
            status, error, request_kind, model, api_key_hash, session_key, session_id,
            ttft_ms, duration_ms, bytes_in, bytes_out, request_headers, response_headers,
            request_body_compressed, response_body_compressed, request_body_bytes, response_body_bytes,
            request_body_truncated, response_body_truncated, content_type, plugin_metadata, tags,
            ttfb_ms, input_tokens, output_tokens, cached_input_tokens, cache_creation_input_tokens, usage_complete, estimated_cost_microusd, tool_calls, tool_call_count
        )
        VALUES (
            $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,
            $21,$22,$23,$24,$25,$26,$27,$28,$29,$30,$31,$32,$33,$34,$35,$36,$37,$38
        )
        "#,
    )
    .bind(trace.id)
    .bind(trace.started_at)
    .bind(trace.completed_at)
    .bind(&trace.method)
    .bind(&trace.original_uri)
    .bind(&trace.upstream_url)
    .bind(&trace.upstream_host)
    .bind(trace.status)
    .bind(&trace.error)
    .bind(trace.request_kind.as_str())
    .bind(&trace.model)
    .bind(&trace.api_key_hash)
    .bind(&trace.session_key)
    .bind(trace.session_id)
    .bind(trace.ttft_ms)
    .bind(trace.duration_ms)
    .bind(trace.bytes_in)
    .bind(trace.bytes_out)
    .bind(&trace.request_headers)
    .bind(&trace.response_headers)
    .bind(Vec::<u8>::new())
    .bind(Vec::<u8>::new())
    .bind(trace.request_body_bytes)
    .bind(trace.response_body_bytes)
    .bind(trace.request_body_truncated)
    .bind(trace.response_body_truncated)
    .bind(&trace.content_type)
    .bind(&trace.plugin_metadata)
    .bind(&trace.tags)
    .bind(trace.ttfb_ms)
    .bind(trace.usage.input_tokens)
    .bind(trace.usage.output_tokens)
    .bind(trace.usage.cached_input_tokens)
    .bind(trace.usage.cache_creation_input_tokens)
    .bind(trace.usage_complete)
    .bind(trace.estimated_cost_microusd)
    .bind(serde_json::to_value(&trace.tool_calls)?)
    .bind(trace.tool_calls.len() as i64)
    .execute(&mut *tx)
    .await?;

    for payload in payloads {
        archive_prepared_payloads(&mut tx, archive, &trace, payload, journal_id).await?;
    }

    update_rollup(&mut tx, &trace).await?;

    if let Some(session_id) = trace.session_id
        && !messages.is_empty()
    {
        let (roles, contents): (Vec<String>, Vec<String>) = messages
            .into_iter()
            .map(|message| (message.role, message.content))
            .unzip();

        sqlx::query(
            r#"
            INSERT INTO session_messages (request_id, session_id, role, content, created_at)
            SELECT $1, $2, message.role, message.content, $5
            FROM UNNEST($3::text[], $4::text[]) AS message(role, content)
            "#,
        )
        .bind(trace.id)
        .bind(session_id)
        .bind(&roles)
        .bind(&contents)
        .bind(trace.started_at)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

pub(super) async fn upsert_session(
    tx: &mut Transaction<'_, Postgres>,
    session_key: &str,
    user_id: Option<String>,
    user_name: Option<String>,
    first_seen: DateTime<Utc>,
    last_seen: DateTime<Utc>,
) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    let row = sqlx::query(
        r#"
        INSERT INTO trace_sessions (id, session_key, first_seen, last_seen, user_id, user_name, summary)
        VALUES ($1, $2, $5, $6, $3, $4, '{}'::jsonb)
        ON CONFLICT (session_key) DO UPDATE
        SET first_seen = LEAST(trace_sessions.first_seen, EXCLUDED.first_seen),
            last_seen = GREATEST(trace_sessions.last_seen, EXCLUDED.last_seen),
            user_id = COALESCE(trace_sessions.user_id, EXCLUDED.user_id),
            user_name = COALESCE(trace_sessions.user_name, EXCLUDED.user_name)
        RETURNING id
        "#,
    )
    .bind(id)
    .bind(session_key)
    .bind(user_id)
    .bind(user_name)
    .bind(first_seen)
    .bind(last_seen)
    .fetch_one(&mut **tx)
    .await?;

    Ok(row.try_get("id")?)
}

pub(super) async fn update_rollup(
    tx: &mut Transaction<'_, Postgres>,
    trace: &TraceRecord,
) -> anyhow::Result<()> {
    let errors = if trace.error.is_some() || trace.status.is_some_and(|status| status >= 400) {
        1
    } else {
        0
    };
    let captured_bytes = trace.request_body_bytes + trace.response_body_bytes;
    let duration_count = if trace.duration_ms.is_some() { 1 } else { 0 };
    let duration_sum_ms = trace.duration_ms.unwrap_or_default();
    let ttft_count = if trace.ttft_ms.is_some() { 1 } else { 0 };
    let ttft_sum_ms = trace.ttft_ms.unwrap_or_default();

    sqlx::query(
        r#"
        INSERT INTO trace_rollups_minute (
            bucket, last_seen, total, errors, captured_bytes,
            duration_count, duration_sum_ms, ttft_count, ttft_sum_ms
        )
        VALUES (
            date_trunc('minute', $1::timestamptz), $1,
            1, $2, $3, $4, $5, $6, $7
        )
        ON CONFLICT (bucket) DO UPDATE
        SET last_seen = GREATEST(trace_rollups_minute.last_seen, EXCLUDED.last_seen),
            total = trace_rollups_minute.total + EXCLUDED.total,
            errors = trace_rollups_minute.errors + EXCLUDED.errors,
            captured_bytes = trace_rollups_minute.captured_bytes + EXCLUDED.captured_bytes,
            duration_count = trace_rollups_minute.duration_count + EXCLUDED.duration_count,
            duration_sum_ms = trace_rollups_minute.duration_sum_ms + EXCLUDED.duration_sum_ms,
            ttft_count = trace_rollups_minute.ttft_count + EXCLUDED.ttft_count,
            ttft_sum_ms = trace_rollups_minute.ttft_sum_ms + EXCLUDED.ttft_sum_ms
        "#,
    )
    .bind(trace.started_at)
    .bind(errors)
    .bind(captured_bytes)
    .bind(duration_count)
    .bind(duration_sum_ms)
    .bind(ttft_count)
    .bind(ttft_sum_ms)
    .execute(&mut **tx)
    .await?;

    Ok(())
}
