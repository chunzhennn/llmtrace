//! Session metadata, message previews and summary reads.
use super::*;

pub(super) const SESSION_REQUEST_STATS_SQL: &str = r#"
        SELECT COUNT(*)::bigint AS request_count,
               COUNT(*) FILTER (WHERE error IS NOT NULL OR status >= 400)::bigint AS error_count,
               COALESCE(SUM(bytes_in), 0)::bigint AS bytes_in,
               COALESCE(SUM(bytes_out), 0)::bigint AS bytes_out,
               COALESCE(SUM(request_body_bytes + response_body_bytes), 0)::bigint AS captured_bytes,
               AVG(duration_ms)::bigint AS avg_duration_ms,
               MAX(duration_ms)::bigint AS max_duration_ms,
               AVG(ttft_ms)::bigint AS avg_ttft_ms,
               MAX(ttft_ms)::bigint AS max_ttft_ms,
               MIN(started_at) AS first_request_at,
               MAX(started_at) AS last_request_at
               , SUM(input_tokens)::bigint AS input_tokens
               , SUM(output_tokens)::bigint AS output_tokens
               , SUM(estimated_cost_microusd)::bigint AS estimated_cost_microusd
               , COALESCE(SUM(tool_call_count), 0)::bigint AS tool_call_count
               , COUNT(*) FILTER (WHERE usage_complete)::bigint AS usage_known_count
               , COUNT(estimated_cost_microusd)::bigint AS priced_request_count
               , COUNT(*) FILTER (WHERE request_body_truncated OR response_body_truncated OR 'session_messages_truncated' = ANY(tags))::bigint AS incomplete_capture_count
        FROM request_traces
        WHERE session_id = $1
        "#;
pub(super) const LIST_SESSIONS_SQL: &str = r#"
        WITH selected_sessions AS (
            SELECT id, session_key, first_seen, last_seen, user_id, user_name
            FROM trace_sessions
            WHERE (
                $1::text IS NULL
                OR session_key ILIKE '%' || $1 || '%' ESCAPE '\'
                OR user_id ILIKE '%' || $1 || '%' ESCAPE '\'
                OR user_name ILIKE '%' || $1 || '%' ESCAPE '\'
            )
            ORDER BY last_seen DESC, id DESC
            LIMIT $2 OFFSET $3
        )
        SELECT s.id, s.session_key, s.first_seen, s.last_seen, s.user_id, s.user_name,
               COALESCE(stats.request_count, 0)::bigint AS request_count,
               COALESCE(stats.max_duration_ms, 0)::bigint AS max_duration_ms
        FROM selected_sessions s
        LEFT JOIN LATERAL (
            SELECT COUNT(*)::bigint AS request_count,
                   COALESCE(MAX(r.duration_ms), 0)::bigint AS max_duration_ms
            FROM request_traces r
            WHERE r.session_id = s.id
        ) stats ON true
        ORDER BY s.last_seen DESC, s.id DESC
        "#;
pub async fn list_sessions(
    pool: &PgPool,
    q: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> anyhow::Result<Value> {
    let q = q.map(|value| escape_like(&value));
    let page = Page::from_query(limit, offset);
    let mut tx = begin_api_read_tx(pool).await?;
    let rows = sqlx::query_as::<_, SessionSummary>(LIST_SESSIONS_SQL)
        .bind(q)
        .bind(page.fetch_limit())
        .bind(page.offset)
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;

    let has_more = rows.len() > page.limit as usize;
    let items: Vec<_> = rows.into_iter().take(page.limit as usize).collect();
    Ok(json!({
        "items": items,
        "page": page.info(has_more),
    }))
}

pub async fn get_session(
    pool: &PgPool,
    id: Uuid,
    messages_limit: Option<i64>,
    messages_offset: Option<i64>,
) -> anyhow::Result<Option<Value>> {
    let mut tx = begin_api_read_tx(pool).await?;
    let session = sqlx::query_as::<_, SessionMetadata>(
        r#"
        SELECT id, session_key, first_seen, last_seen, user_id, user_name, summary
        FROM trace_sessions
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(session) = session else {
        tx.commit().await?;
        return Ok(None);
    };

    let page = Page::from_query(messages_limit, messages_offset);
    let request_stats = sqlx::query_as::<_, SessionRequestStats>(SESSION_REQUEST_STATS_SQL)
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    let messages = sqlx::query_as::<_, SessionMessage>(
        r#"
        SELECT id, request_id, role, content, created_at
        FROM session_messages
        WHERE session_id = $1
        ORDER BY created_at ASC, id ASC
        LIMIT $2 OFFSET $3
        "#,
    )
    .bind(id)
    .bind(page.fetch_limit())
    .bind(page.offset)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let has_more = messages.len() > page.limit as usize;
    let messages = messages.into_iter().take(page.limit as usize).collect();
    Ok(Some(serde_json::to_value(SessionDetail {
        session,
        request_stats,
        messages,
        messages_page: page.info(has_more),
    })?))
}
