//! Request listings, facets and typed request detail reads.
use super::*;

pub(super) const REQUEST_FACET_MODELS_SQL: &str = r#"
        SELECT model AS value, COUNT(*)::bigint AS request_count
        FROM trace_requests
        WHERE started_at >= $1
          AND model IS NOT NULL
          AND model <> ''
        GROUP BY model
        ORDER BY request_count DESC, value ASC
        LIMIT $2
        "#;
pub(super) const REQUEST_FACET_UPSTREAM_HOSTS_SQL: &str = r#"
        SELECT upstream_host AS value, COUNT(*)::bigint AS request_count
        FROM trace_requests
        WHERE started_at >= $1
          AND upstream_host IS NOT NULL
          AND upstream_host <> ''
        GROUP BY upstream_host
        ORDER BY request_count DESC, value ASC
        LIMIT $2
        "#;
pub(super) const REQUEST_FACET_REQUEST_KINDS_SQL: &str = r#"
        SELECT request_kind AS value, COUNT(*)::bigint AS request_count
        FROM trace_requests
        WHERE started_at >= $1
          AND request_kind IS NOT NULL
          AND request_kind <> ''
        GROUP BY request_kind
        ORDER BY request_count DESC, value ASC
        LIMIT $2
        "#;
pub(super) const REQUEST_FACET_STATUSES_SQL: &str = r#"
        SELECT status AS value, COUNT(*)::bigint AS request_count
        FROM trace_requests
        WHERE started_at >= $1
          AND status IS NOT NULL
        GROUP BY status
        ORDER BY request_count DESC, value ASC
        LIMIT $2
        "#;
pub(super) const REQUEST_FACET_STATUS_CLASSES_SQL: &str = r#"
        SELECT CASE
                   WHEN status IS NULL THEN 'no_status'
                   WHEN status BETWEEN 100 AND 199 THEN '1xx'
                   WHEN status BETWEEN 200 AND 299 THEN '2xx'
                   WHEN status BETWEEN 300 AND 399 THEN '3xx'
                   WHEN status BETWEEN 400 AND 499 THEN '4xx'
                   WHEN status BETWEEN 500 AND 599 THEN '5xx'
                   ELSE 'other'
               END AS value,
               COUNT(*)::bigint AS request_count
        FROM trace_requests
        WHERE started_at >= $1
        GROUP BY 1
        ORDER BY request_count DESC, value ASC
        LIMIT $2
        "#;
pub(super) const REQUEST_FACET_ERROR_STATES_SQL: &str = r#"
        SELECT (error IS NOT NULL OR COALESCE(status >= 400, false)) AS value,
               COUNT(*)::bigint AS request_count
        FROM trace_requests
        WHERE started_at >= $1
        GROUP BY 1
        ORDER BY value DESC
        "#;
pub(super) const LIST_REQUESTS_SQL: &str = r#"
        SELECT id, started_at, completed_at, method, original_uri, upstream_url, upstream_host,
               status, error, request_kind, model, api_key_hash, session_id, ttft_ms,
               duration_ms, bytes_in, bytes_out, request_body_truncated, response_body_truncated,
               plugin_metadata, tags, ttfb_ms, input_tokens, output_tokens, cached_input_tokens, cache_creation_input_tokens, usage_complete, estimated_cost_microusd, tool_call_count
        FROM trace_requests
        WHERE (
            $1::text IS NULL
            OR upstream_url ILIKE '%' || $1 || '%' ESCAPE '\'
            OR model ILIKE '%' || $1 || '%' ESCAPE '\'
            OR request_kind ILIKE '%' || $1 || '%' ESCAPE '\'
        )
          AND ($2::int IS NULL OR status = $2)
          AND ($3::text IS NULL OR upstream_host = $3)
          AND ($4::text IS NULL OR model = $4)
          AND ($5::text IS NULL OR request_kind = $5)
          AND ($6::uuid IS NULL OR session_id = $6)
          AND ($7::text IS NULL OR api_key_hash = $7)
          AND ($8::timestamptz IS NULL OR started_at >= $8)
          AND ($9::timestamptz IS NULL OR started_at <= $9)
          AND ($10::bigint IS NULL OR duration_ms >= $10)
          AND ($11::bigint IS NULL OR duration_ms <= $11)
          AND (
              $12::boolean IS NULL
              OR ($12 = true AND (error IS NOT NULL OR status >= 400))
              OR ($12 = false AND error IS NULL AND (status IS NULL OR status < 400))
          )
          AND (
              $13::text IS NULL
              OR ($13 = 'no_status' AND status IS NULL)
              OR ($13 = '1xx' AND status BETWEEN 100 AND 199)
              OR ($13 = '2xx' AND status BETWEEN 200 AND 299)
              OR ($13 = '3xx' AND status BETWEEN 300 AND 399)
              OR ($13 = '4xx' AND status BETWEEN 400 AND 499)
              OR ($13 = '5xx' AND status BETWEEN 500 AND 599)
              OR ($13 = 'other' AND status IS NOT NULL AND (status < 100 OR status > 599))
          )
        ORDER BY started_at DESC, id DESC
        LIMIT $14 OFFSET $15
        "#;
pub(super) const LIST_SESSION_REQUESTS_SQL: &str = r#"
        SELECT id, started_at, completed_at, method, original_uri, upstream_url, upstream_host,
               status, error, request_kind, model, api_key_hash, session_id, ttft_ms,
               duration_ms, bytes_in, bytes_out, request_body_truncated, response_body_truncated,
               plugin_metadata, tags, ttfb_ms, input_tokens, output_tokens, cached_input_tokens, cache_creation_input_tokens, usage_complete, estimated_cost_microusd, tool_call_count
        FROM trace_requests
        WHERE session_id = $1
        ORDER BY started_at DESC, id DESC
        LIMIT $2 OFFSET $3
        "#;
pub(super) const RECENT_ERROR_REQUESTS_SQL: &str = r#"
        SELECT id, started_at, completed_at, method, original_uri, upstream_url, upstream_host,
               status, error, request_kind, model, api_key_hash, session_id, ttft_ms,
               duration_ms, bytes_in, bytes_out, request_body_truncated, response_body_truncated,
               plugin_metadata, tags, ttfb_ms, input_tokens, output_tokens, cached_input_tokens, cache_creation_input_tokens, usage_complete, estimated_cost_microusd, tool_call_count
        FROM trace_requests
        WHERE started_at >= $1
          AND (error IS NOT NULL OR status >= 400)
        ORDER BY started_at DESC, id DESC
        LIMIT $2
        "#;
pub(super) const SLOW_REQUESTS_SQL: &str = r#"
        SELECT id, started_at, completed_at, method, original_uri, upstream_url, upstream_host,
               status, error, request_kind, model, api_key_hash, session_id, ttft_ms,
               duration_ms, bytes_in, bytes_out, request_body_truncated, response_body_truncated,
               plugin_metadata, tags, ttfb_ms, input_tokens, output_tokens, cached_input_tokens, cache_creation_input_tokens, usage_complete, estimated_cost_microusd, tool_call_count
        FROM trace_requests
        WHERE started_at >= $1
          AND duration_ms >= $2
        ORDER BY duration_ms DESC, started_at DESC, id DESC
        LIMIT $3 OFFSET $4
        "#;
#[derive(Debug, Clone, Default)]
pub struct RequestListFilters {
    pub q: Option<String>,
    pub status: Option<i32>,
    pub status_class: Option<String>,
    pub has_error: Option<bool>,
    pub upstream_host: Option<String>,
    pub model: Option<String>,
    pub request_kind: Option<String>,
    pub session_id: Option<Uuid>,
    pub api_key_hash: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub min_duration_ms: Option<i64>,
    pub max_duration_ms: Option<i64>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

pub async fn list_requests(pool: &PgPool, filters: RequestListFilters) -> anyhow::Result<Value> {
    let q = filters.q.map(|value| escape_like(&value));
    let page = Page::from_query(filters.limit, filters.offset);
    let mut tx = begin_api_read_tx(pool).await?;
    let rows = sqlx::query_as::<_, RequestSummary>(LIST_REQUESTS_SQL)
        .bind(q)
        .bind(filters.status)
        .bind(filters.upstream_host)
        .bind(filters.model)
        .bind(filters.request_kind)
        .bind(filters.session_id)
        .bind(filters.api_key_hash)
        .bind(filters.since)
        .bind(filters.until)
        .bind(filters.min_duration_ms)
        .bind(filters.max_duration_ms)
        .bind(filters.has_error)
        .bind(filters.status_class)
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

pub async fn list_session_requests(
    pool: &PgPool,
    session_id: Uuid,
    limit: Option<i64>,
    offset: Option<i64>,
) -> anyhow::Result<Option<Value>> {
    let page = Page::from_query(limit, offset);
    let mut tx = begin_api_read_tx(pool).await?;
    let session_exists = sqlx::query("SELECT 1 FROM trace_sessions WHERE id = $1")
        .bind(session_id)
        .fetch_optional(&mut *tx)
        .await?
        .is_some();
    if !session_exists {
        tx.commit().await?;
        return Ok(None);
    }

    let rows = sqlx::query_as::<_, RequestSummary>(LIST_SESSION_REQUESTS_SQL)
        .bind(session_id)
        .bind(page.fetch_limit())
        .bind(page.offset)
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;

    let has_more = rows.len() > page.limit as usize;
    let items: Vec<_> = rows.into_iter().take(page.limit as usize).collect();

    Ok(Some(json!({
        "session_id": session_id,
        "items": items,
        "page": page.info(has_more),
    })))
}

pub async fn recent_error_requests(
    pool: &PgPool,
    since_hours: Option<i64>,
    limit: Option<i64>,
) -> anyhow::Result<Value> {
    let window = RECENT_ERROR_WINDOW.resolve(since_hours, limit);
    let cutoff = window.cutoff(Utc::now());
    let mut tx = begin_api_read_tx(pool).await?;
    let rows = sqlx::query_as::<_, RequestSummary>(RECENT_ERROR_REQUESTS_SQL)
        .bind(cutoff)
        .bind(window.fetch_limit())
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;

    let has_more = rows.len() > window.limit as usize;
    let items: Vec<_> = rows.into_iter().take(window.limit as usize).collect();

    Ok(json!({
        "window": window.metadata(cutoff),
        "items": items,
        "page": {
            "limit": window.limit,
            "has_more": has_more,
        },
    }))
}

pub async fn slow_requests(
    pool: &PgPool,
    since_hours: Option<i64>,
    min_duration_ms: Option<i64>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> anyhow::Result<Value> {
    let window = LookbackWindow::from_query(since_hours);
    let page = Page::from_query(limit.or(Some(50)), offset);
    let min_duration_ms = min_duration_ms.unwrap_or(1000).max(0);
    let cutoff = window.cutoff(Utc::now());
    let mut tx = begin_api_read_tx(pool).await?;
    let rows = sqlx::query_as::<_, RequestSummary>(SLOW_REQUESTS_SQL)
        .bind(cutoff)
        .bind(min_duration_ms)
        .bind(page.fetch_limit())
        .bind(page.offset)
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;

    let has_more = rows.len() > page.limit as usize;
    let items: Vec<_> = rows.into_iter().take(page.limit as usize).collect();

    Ok(json!({
        "window": {
            "since_hours": window.since_hours,
            "started_at_gte": cutoff,
            "min_duration_ms": min_duration_ms,
        },
        "items": items,
        "page": page.info(has_more),
    }))
}

pub async fn request_facets(
    pool: &PgPool,
    since_hours: Option<i64>,
    limit: Option<i64>,
) -> anyhow::Result<Value> {
    let window = REQUEST_FACET_WINDOW.resolve(since_hours, limit);
    let cutoff = window.cutoff(Utc::now());
    let mut tx = begin_api_read_tx(pool).await?;

    let models = sqlx::query(REQUEST_FACET_MODELS_SQL)
        .bind(cutoff)
        .bind(window.limit)
        .fetch_all(&mut *tx)
        .await?;
    let upstream_hosts = sqlx::query(REQUEST_FACET_UPSTREAM_HOSTS_SQL)
        .bind(cutoff)
        .bind(window.limit)
        .fetch_all(&mut *tx)
        .await?;
    let request_kinds = sqlx::query(REQUEST_FACET_REQUEST_KINDS_SQL)
        .bind(cutoff)
        .bind(window.limit)
        .fetch_all(&mut *tx)
        .await?;
    let statuses = sqlx::query(REQUEST_FACET_STATUSES_SQL)
        .bind(cutoff)
        .bind(window.limit)
        .fetch_all(&mut *tx)
        .await?;
    let status_classes = sqlx::query(REQUEST_FACET_STATUS_CLASSES_SQL)
        .bind(cutoff)
        .bind(window.limit)
        .fetch_all(&mut *tx)
        .await?;
    let error_states = sqlx::query(REQUEST_FACET_ERROR_STATES_SQL)
        .bind(cutoff)
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;

    Ok(json!({
        "window": window.metadata(cutoff),
        "facets": {
            "models": read_rows::<Facet<String>>(models)?,
            "upstream_hosts": read_rows::<Facet<String>>(upstream_hosts)?,
            "request_kinds": read_rows::<Facet<String>>(request_kinds)?,
            "statuses": read_rows::<Facet<i32>>(statuses)?,
            "status_classes": read_rows::<Facet<String>>(status_classes)?,
            "error_states": read_rows::<Facet<bool>>(error_states)?,
        },
    }))
}

pub async fn get_request(
    pool: &PgPool,
    id: Uuid,
    archive: &ArchiveConfig,
    include_bodies: bool,
) -> anyhow::Result<Option<Value>> {
    let mut tx = begin_api_snapshot_tx(pool).await?;
    let request = sqlx::query_as::<_, RequestMetadata>(&format!(
        "SELECT {REQUEST_METADATA_COLUMNS} FROM request_traces WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(mut request) = request else {
        return Ok(None);
    };
    let bodies = if include_bodies {
        Some(bodies::load_trace_bodies(&mut tx, archive, &request).await?)
    } else {
        None
    };
    tx.commit().await?;
    let (request_body, response_body, request_body_status, response_body_status) = match bodies {
        Some(bodies) => {
            request.summary.request_body_truncated = bodies.request.truncated;
            request.summary.response_body_truncated = bodies.response.truncated;
            (
                bodies.request.text(),
                bodies.response.text(),
                Some(bodies.request.status),
                Some(bodies.response.status),
            )
        }
        None => (String::new(), String::new(), None, None),
    };
    Ok(Some(serde_json::to_value(RequestDetail {
        request,
        bodies_included: include_bodies,
        request_body,
        response_body,
        request_body_status,
        response_body_status,
    })?))
}
