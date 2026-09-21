//! Read-only usage, latency and integrity aggregations.
use super::*;

#[derive(Clone, Copy)]
enum GroupBy {
    Model,
    Upstream,
    RequestKind,
    RawRequestKind,
    StatusClass,
    ErrorSource,
}
impl GroupBy {
    fn sql(self) -> &'static str {
        match self {
            Self::Model => "COALESCE(NULLIF(model, ''), 'unknown')",
            Self::Upstream => "COALESCE(NULLIF(upstream_host, ''), 'unknown')",
            Self::RequestKind => "COALESCE(NULLIF(request_kind, ''), 'unknown')",
            Self::RawRequestKind => "request_kind",
            Self::StatusClass => STATUS_CLASS_SQL,
            Self::ErrorSource => ERROR_SOURCE_SQL,
        }
    }
}
#[derive(Clone, Copy)]
enum Breakdown {
    Usage,
    Errors,
    Latency,
}
impl Breakdown {
    fn query(
        self,
        group: GroupBy,
        cutoff: DateTime<Utc>,
        limit: i64,
    ) -> QueryBuilder<'static, Postgres> {
        let (fields, filter, order) = match self {
            Self::Usage => (USAGE_COLUMNS, "", " ORDER BY request_count DESC, name ASC"),
            Self::Errors => (
                ERROR_COLUMNS,
                " AND (error IS NOT NULL OR status >= 400)",
                " ORDER BY error_count DESC, name ASC",
            ),
            Self::Latency => (
                LATENCY_COLUMNS,
                "",
                " HAVING COUNT(duration_ms) > 0 OR COUNT(ttft_ms) > 0 ORDER BY p95_duration_ms DESC NULLS LAST, request_count DESC, name ASC",
            ),
        };
        let mut query = QueryBuilder::new(format!(
            "SELECT {} AS name, {fields} FROM trace_requests WHERE started_at >= ",
            group.sql()
        ));
        query
            .push_bind(cutoff)
            .push(filter)
            .push(" GROUP BY 1")
            .push(order)
            .push(" LIMIT ")
            .push_bind(limit);
        query
    }
}

async fn breakdowns<T>(
    tx: &mut Transaction<'_, Postgres>,
    kind: Breakdown,
    cutoff: DateTime<Utc>,
    limit: i64,
    groups: &[(&str, GroupBy)],
) -> anyhow::Result<serde_json::Map<String, Value>>
where
    T: for<'r> FromRow<'r, sqlx::postgres::PgRow> + Serialize + Send + Unpin,
{
    let mut result = serde_json::Map::new();
    for (name, group) in groups {
        let rows = kind
            .query(*group, cutoff, limit)
            .build_query_as::<T>()
            .fetch_all(&mut **tx)
            .await?;
        result.insert((*name).into(), serde_json::to_value(rows)?);
    }
    Ok(result)
}

const ERROR_SOURCE_SQL: &str = r#"CASE
                   WHEN error IS NOT NULL AND status >= 500 THEN 'proxy_error_and_http_5xx'
                   WHEN error IS NOT NULL THEN 'proxy_error'
                   WHEN status >= 500 THEN 'http_5xx'
                   WHEN status >= 400 THEN 'http_4xx'
                   ELSE 'other'
               END"#;
const USAGE_COLUMNS: &str = r#"COUNT(*)::bigint AS request_count,
               COUNT(*) FILTER (WHERE error IS NOT NULL OR status >= 400)::bigint AS error_count,
               AVG(duration_ms)::bigint AS avg_duration_ms,
               AVG(ttft_ms)::bigint AS avg_ttft_ms"#;
const ERROR_COLUMNS: &str = r#"COUNT(*)::bigint AS error_count,
               COUNT(*) FILTER (WHERE error IS NOT NULL)::bigint AS proxy_error_count,
               COUNT(*) FILTER (WHERE status >= 500)::bigint AS http_5xx_count,
               AVG(duration_ms)::bigint AS avg_duration_ms,
               MAX(duration_ms)::bigint AS max_duration_ms"#;
const LATENCY_COLUMNS: &str = r#"COUNT(*)::bigint AS request_count,
               COUNT(duration_ms)::bigint AS duration_count,
               AVG(duration_ms)::bigint AS avg_duration_ms,
               MAX(duration_ms)::bigint AS max_duration_ms,
               (percentile_cont(0.50) WITHIN GROUP (ORDER BY duration_ms))::bigint AS p50_duration_ms,
               (percentile_cont(0.90) WITHIN GROUP (ORDER BY duration_ms))::bigint AS p90_duration_ms,
               (percentile_cont(0.95) WITHIN GROUP (ORDER BY duration_ms))::bigint AS p95_duration_ms,
               (percentile_cont(0.99) WITHIN GROUP (ORDER BY duration_ms))::bigint AS p99_duration_ms,
               COUNT(ttft_ms)::bigint AS ttft_count,
               AVG(ttft_ms)::bigint AS avg_ttft_ms,
               MAX(ttft_ms)::bigint AS max_ttft_ms,
               (percentile_cont(0.50) WITHIN GROUP (ORDER BY ttft_ms))::bigint AS p50_ttft_ms,
               (percentile_cont(0.90) WITHIN GROUP (ORDER BY ttft_ms))::bigint AS p90_ttft_ms,
               (percentile_cont(0.95) WITHIN GROUP (ORDER BY ttft_ms))::bigint AS p95_ttft_ms,
               (percentile_cont(0.99) WITHIN GROUP (ORDER BY ttft_ms))::bigint AS p99_ttft_ms"#;

pub(super) const ERROR_SUMMARY_TOTALS_SQL: &str = r#"
        SELECT COUNT(*)::bigint AS error_count,
               COUNT(*) FILTER (WHERE error IS NOT NULL)::bigint AS proxy_error_count,
               COUNT(*) FILTER (WHERE status >= 500)::bigint AS http_5xx_count,
               COUNT(DISTINCT session_id) FILTER (WHERE session_id IS NOT NULL)::bigint AS affected_sessions,
               AVG(duration_ms)::bigint AS avg_duration_ms,
               MAX(duration_ms)::bigint AS max_duration_ms,
               MIN(started_at) AS first_seen_at,
               MAX(started_at) AS last_seen_at
        FROM trace_requests
        WHERE started_at >= $1
          AND (error IS NOT NULL OR status >= 400)
        "#;
pub(super) const UPSTREAM_HEALTH_SQL: &str = r#"
        SELECT COALESCE(NULLIF(upstream_host, ''), 'unknown') AS upstream_host,
               COUNT(*)::bigint AS request_count,
               COUNT(*) FILTER (WHERE error IS NOT NULL OR status >= 400)::bigint AS error_count,
               COUNT(*) FILTER (WHERE error IS NOT NULL)::bigint AS proxy_error_count,
               COUNT(*) FILTER (WHERE status BETWEEN 200 AND 299)::bigint AS http_2xx_count,
               COUNT(*) FILTER (WHERE status BETWEEN 300 AND 399)::bigint AS http_3xx_count,
               COUNT(*) FILTER (WHERE status BETWEEN 400 AND 499)::bigint AS http_4xx_count,
               COUNT(*) FILTER (WHERE status >= 500)::bigint AS http_5xx_count,
               COUNT(*) FILTER (WHERE status IS NULL)::bigint AS no_status_count,
               COUNT(DISTINCT session_id) FILTER (WHERE session_id IS NOT NULL)::bigint AS session_count,
               COALESCE(SUM(bytes_in), 0)::bigint AS bytes_in,
               COALESCE(SUM(bytes_out), 0)::bigint AS bytes_out,
               COALESCE(SUM(request_body_bytes + response_body_bytes), 0)::bigint AS captured_bytes,
               COUNT(duration_ms)::bigint AS duration_count,
               AVG(duration_ms)::bigint AS avg_duration_ms,
               MAX(duration_ms)::bigint AS max_duration_ms,
               (percentile_cont(0.95) WITHIN GROUP (ORDER BY duration_ms))::bigint AS p95_duration_ms,
               COUNT(ttft_ms)::bigint AS ttft_count,
               AVG(ttft_ms)::bigint AS avg_ttft_ms,
               MAX(ttft_ms)::bigint AS max_ttft_ms,
               (percentile_cont(0.95) WITHIN GROUP (ORDER BY ttft_ms))::bigint AS p95_ttft_ms,
               MIN(started_at) AS first_seen_at,
               MAX(started_at) AS last_seen_at
        FROM trace_requests
        WHERE started_at >= $1
        GROUP BY 1
        ORDER BY error_count DESC, p95_duration_ms DESC NULLS LAST, request_count DESC, upstream_host ASC
        LIMIT $2
        "#;
pub(super) const MODEL_USAGE_SQL: &str = r#"
        SELECT COALESCE(NULLIF(model, ''), 'unknown') AS model,
               COUNT(*)::bigint AS request_count,
               COUNT(*) FILTER (WHERE error IS NOT NULL OR status >= 400)::bigint AS error_count,
               COUNT(*) FILTER (WHERE error IS NOT NULL)::bigint AS proxy_error_count,
               COUNT(*) FILTER (WHERE status >= 500)::bigint AS http_5xx_count,
               COUNT(DISTINCT upstream_host) FILTER (WHERE upstream_host IS NOT NULL AND upstream_host <> '')::bigint AS upstream_count,
               COUNT(DISTINCT api_key_hash) FILTER (WHERE api_key_hash IS NOT NULL AND api_key_hash <> '')::bigint AS api_key_count,
               COUNT(DISTINCT session_id) FILTER (WHERE session_id IS NOT NULL)::bigint AS session_count,
               COALESCE(SUM(bytes_in), 0)::bigint AS bytes_in,
               COALESCE(SUM(bytes_out), 0)::bigint AS bytes_out,
               COALESCE(SUM(request_body_bytes + response_body_bytes), 0)::bigint AS captured_bytes,
               COUNT(duration_ms)::bigint AS duration_count,
               AVG(duration_ms)::bigint AS avg_duration_ms,
               MAX(duration_ms)::bigint AS max_duration_ms,
               (percentile_cont(0.95) WITHIN GROUP (ORDER BY duration_ms))::bigint AS p95_duration_ms,
               COUNT(ttft_ms)::bigint AS ttft_count,
               AVG(ttft_ms)::bigint AS avg_ttft_ms,
               MAX(ttft_ms)::bigint AS max_ttft_ms,
               (percentile_cont(0.95) WITHIN GROUP (ORDER BY ttft_ms))::bigint AS p95_ttft_ms,
               MIN(started_at) AS first_seen_at,
               MAX(started_at) AS last_seen_at
               , SUM(input_tokens)::bigint AS input_tokens
               , SUM(output_tokens)::bigint AS output_tokens
               , SUM(estimated_cost_microusd)::bigint AS estimated_cost_microusd
               , SUM(tool_call_count)::bigint AS tool_call_count
               , COUNT(*) FILTER (WHERE usage_complete)::bigint AS usage_known_count
               , COUNT(estimated_cost_microusd)::bigint AS priced_request_count
        FROM trace_requests
        WHERE started_at >= $1
        GROUP BY 1
        ORDER BY request_count DESC, error_count DESC, model ASC
        LIMIT $2
        "#;
pub(super) const USER_USAGE_SQL: &str = r#"
        SELECT COALESCE(NULLIF(s.user_id, ''), 'unknown') AS user_id,
               COALESCE(NULLIF(s.user_name, ''), 'unknown') AS user_name,
               COUNT(*)::bigint AS request_count,
               COUNT(*) FILTER (WHERE r.error IS NOT NULL OR r.status >= 400)::bigint AS error_count,
               COUNT(*) FILTER (WHERE r.error IS NOT NULL)::bigint AS proxy_error_count,
               COUNT(*) FILTER (WHERE r.status >= 500)::bigint AS http_5xx_count,
               COUNT(DISTINCT r.session_id) FILTER (WHERE r.session_id IS NOT NULL)::bigint AS session_count,
               COUNT(DISTINCT r.api_key_hash) FILTER (WHERE r.api_key_hash IS NOT NULL AND r.api_key_hash <> '')::bigint AS api_key_count,
               COUNT(DISTINCT r.upstream_host) FILTER (WHERE r.upstream_host IS NOT NULL AND r.upstream_host <> '')::bigint AS upstream_count,
               COALESCE(SUM(r.bytes_in), 0)::bigint AS bytes_in,
               COALESCE(SUM(r.bytes_out), 0)::bigint AS bytes_out,
               COALESCE(SUM(r.request_body_bytes + r.response_body_bytes), 0)::bigint AS captured_bytes,
               COUNT(r.duration_ms)::bigint AS duration_count,
               AVG(r.duration_ms)::bigint AS avg_duration_ms,
               MAX(r.duration_ms)::bigint AS max_duration_ms,
               (percentile_cont(0.95) WITHIN GROUP (ORDER BY r.duration_ms))::bigint AS p95_duration_ms,
               COUNT(r.ttft_ms)::bigint AS ttft_count,
               AVG(r.ttft_ms)::bigint AS avg_ttft_ms,
               MAX(r.ttft_ms)::bigint AS max_ttft_ms,
               (percentile_cont(0.95) WITHIN GROUP (ORDER BY r.ttft_ms))::bigint AS p95_ttft_ms,
               MIN(r.started_at) AS first_seen_at,
               MAX(r.started_at) AS last_seen_at
               , SUM(r.input_tokens)::bigint AS input_tokens
               , SUM(r.output_tokens)::bigint AS output_tokens
               , SUM(r.estimated_cost_microusd)::bigint AS estimated_cost_microusd
               , SUM(r.tool_call_count)::bigint AS tool_call_count
               , COUNT(*) FILTER (WHERE r.usage_complete)::bigint AS usage_known_count
               , COUNT(r.estimated_cost_microusd)::bigint AS priced_request_count
        FROM trace_requests r
        LEFT JOIN trace_sessions s ON s.id = r.session_id
        WHERE r.started_at >= $1
        GROUP BY 1, 2
        ORDER BY request_count DESC, user_id ASC, user_name ASC
        LIMIT $2
        "#;
pub(super) const API_KEY_USAGE_SQL: &str = r#"
        SELECT api_key_hash,
               COUNT(*)::bigint AS request_count,
               COUNT(*) FILTER (WHERE error IS NOT NULL OR status >= 400)::bigint AS error_count,
               COUNT(DISTINCT session_id) FILTER (WHERE session_id IS NOT NULL)::bigint AS session_count,
               COALESCE(SUM(bytes_in), 0)::bigint AS bytes_in,
               COALESCE(SUM(bytes_out), 0)::bigint AS bytes_out,
               COALESCE(SUM(request_body_bytes + response_body_bytes), 0)::bigint AS captured_bytes,
               AVG(duration_ms)::bigint AS avg_duration_ms,
               MAX(duration_ms)::bigint AS max_duration_ms,
               AVG(ttft_ms)::bigint AS avg_ttft_ms,
               MAX(ttft_ms)::bigint AS max_ttft_ms,
               MIN(started_at) AS first_seen_at,
               MAX(started_at) AS last_seen_at
               , SUM(input_tokens)::bigint AS input_tokens
               , SUM(output_tokens)::bigint AS output_tokens
               , SUM(estimated_cost_microusd)::bigint AS estimated_cost_microusd
               , SUM(tool_call_count)::bigint AS tool_call_count
               , COUNT(*) FILTER (WHERE usage_complete)::bigint AS usage_known_count
               , COUNT(estimated_cost_microusd)::bigint AS priced_request_count
        FROM trace_requests
        WHERE started_at >= $1
          AND api_key_hash IS NOT NULL
          AND api_key_hash <> ''
        GROUP BY api_key_hash
        ORDER BY request_count DESC, api_key_hash ASC
        LIMIT $2
        "#;
pub(super) const DATA_OVERVIEW_ROLLUPS_SQL: &str = r#"
        SELECT COALESCE(SUM(total), 0)::bigint AS request_count,
               COALESCE(SUM(errors), 0)::bigint AS error_count,
               COALESCE(SUM(captured_bytes), 0)::bigint AS captured_bytes,
               MIN(bucket) AS first_bucket_at,
               MAX(bucket) AS last_bucket_at,
               MAX(last_seen) AS last_seen_at
        FROM trace_rollups_minute
        "#;
pub(super) const DATA_OVERVIEW_RECENT_REQUESTS_SQL: &str = r#"
        SELECT COUNT(*)::bigint AS request_count,
               COUNT(*) FILTER (WHERE error IS NOT NULL OR status >= 400)::bigint AS error_count,
               COUNT(*) FILTER (WHERE error IS NOT NULL)::bigint AS proxy_error_count,
               COUNT(*) FILTER (WHERE status >= 500)::bigint AS http_5xx_count,
               COUNT(DISTINCT upstream_host) FILTER (WHERE upstream_host IS NOT NULL AND upstream_host <> '')::bigint AS upstream_count,
               COUNT(DISTINCT model) FILTER (WHERE model IS NOT NULL AND model <> '')::bigint AS model_count,
               COUNT(DISTINCT session_id) FILTER (WHERE session_id IS NOT NULL)::bigint AS session_count,
               COALESCE(SUM(bytes_in), 0)::bigint AS bytes_in,
               COALESCE(SUM(bytes_out), 0)::bigint AS bytes_out,
               COALESCE(SUM(request_body_bytes + response_body_bytes), 0)::bigint AS captured_bytes,
               AVG(duration_ms)::bigint AS avg_duration_ms,
               MAX(duration_ms)::bigint AS max_duration_ms,
               AVG(ttft_ms)::bigint AS avg_ttft_ms,
               MAX(ttft_ms)::bigint AS max_ttft_ms,
               MIN(started_at) AS first_seen_at,
               MAX(started_at) AS last_seen_at
        FROM trace_requests
        WHERE started_at >= $1
        "#;
pub(super) const DATA_OVERVIEW_SESSIONS_SQL: &str = r#"
        SELECT COUNT(*)::bigint AS session_count,
               COUNT(*) FILTER (WHERE last_seen >= $1)::bigint AS active_session_count,
               MIN(first_seen) AS first_seen_at,
               MAX(last_seen) AS last_seen_at
        FROM trace_sessions
        "#;
pub(super) const DATA_OVERVIEW_AUDIT_SQL: &str = r#"
        SELECT COUNT(*)::bigint AS event_count,
               COUNT(*) FILTER (WHERE created_at >= $1)::bigint AS recent_event_count,
               COUNT(DISTINCT event_type) FILTER (WHERE event_type IS NOT NULL AND event_type <> '')::bigint AS event_type_count,
               MIN(created_at) AS first_seen_at,
               MAX(created_at) AS last_seen_at
        FROM ui_audit_events
        "#;
pub(super) const DATA_OVERVIEW_AUTH_STATE_SQL: &str = r#"
        SELECT (
                   SELECT COUNT(*)::bigint
                   FROM ui_sessions
                   WHERE expires_at >= $1
               ) AS active_ui_sessions,
               (
                   SELECT COUNT(*)::bigint
                   FROM ui_sessions
                   WHERE expires_at < $1
               ) AS expired_ui_sessions,
               (
                   SELECT COUNT(*)::bigint
                   FROM oauth_states
                   WHERE expires_at >= $1
               ) AS pending_oauth_states,
               (
                   SELECT COUNT(*)::bigint
                   FROM oauth_states
                   WHERE expires_at < $1
               ) AS expired_oauth_states
        "#;
macro_rules! data_integrity_rollup_diffs_sql {
    ($select:literal) => {
        concat!(
            r#"
        WITH bounds AS (
            SELECT date_trunc('minute', $1::timestamptz) AS start_bucket,
                   date_trunc('minute', $2::timestamptz) AS end_bucket
        ),
        raw AS (
            SELECT date_trunc('minute', started_at) AS bucket,
                   COUNT(*)::bigint AS total,
                   COUNT(*) FILTER (WHERE error IS NOT NULL OR status >= 400)::bigint AS errors,
                   COALESCE(SUM(request_body_bytes + response_body_bytes), 0)::bigint AS captured_bytes,
                   COUNT(duration_ms)::bigint AS duration_count,
                   COALESCE(SUM(duration_ms), 0)::bigint AS duration_sum_ms,
                   COUNT(ttft_ms)::bigint AS ttft_count,
                   COALESCE(SUM(ttft_ms), 0)::bigint AS ttft_sum_ms
            FROM request_traces, bounds
            WHERE started_at >= bounds.start_bucket
              AND started_at <= $2
            GROUP BY 1
        ),
        rollups AS (
            SELECT bucket,
                   COALESCE(SUM(total), 0)::bigint AS total,
                   COALESCE(SUM(errors), 0)::bigint AS errors,
                   COALESCE(SUM(captured_bytes), 0)::bigint AS captured_bytes,
                   COALESCE(SUM(duration_count), 0)::bigint AS duration_count,
                   COALESCE(SUM(duration_sum_ms), 0)::bigint AS duration_sum_ms,
                   COALESCE(SUM(ttft_count), 0)::bigint AS ttft_count,
                   COALESCE(SUM(ttft_sum_ms), 0)::bigint AS ttft_sum_ms
            FROM trace_rollups_minute, bounds
            WHERE bucket >= bounds.start_bucket
              AND bucket <= bounds.end_bucket
            GROUP BY bucket
        ),
        combined AS (
            SELECT COALESCE(raw.bucket, rollups.bucket) AS bucket,
                   raw.bucket IS NOT NULL AS has_raw,
                   rollups.bucket IS NOT NULL AS has_rollup,
                   COALESCE(raw.total, 0)::bigint AS raw_total,
                   COALESCE(rollups.total, 0)::bigint AS rollup_total,
                   COALESCE(raw.errors, 0)::bigint AS raw_errors,
                   COALESCE(rollups.errors, 0)::bigint AS rollup_errors,
                   COALESCE(raw.captured_bytes, 0)::bigint AS raw_captured_bytes,
                   COALESCE(rollups.captured_bytes, 0)::bigint AS rollup_captured_bytes,
                   COALESCE(raw.duration_count, 0)::bigint AS raw_duration_count,
                   COALESCE(rollups.duration_count, 0)::bigint AS rollup_duration_count,
                   COALESCE(raw.duration_sum_ms, 0)::bigint AS raw_duration_sum_ms,
                   COALESCE(rollups.duration_sum_ms, 0)::bigint AS rollup_duration_sum_ms,
                   COALESCE(raw.ttft_count, 0)::bigint AS raw_ttft_count,
                   COALESCE(rollups.ttft_count, 0)::bigint AS rollup_ttft_count,
                   COALESCE(raw.ttft_sum_ms, 0)::bigint AS raw_ttft_sum_ms,
                   COALESCE(rollups.ttft_sum_ms, 0)::bigint AS rollup_ttft_sum_ms
            FROM raw
            FULL OUTER JOIN rollups USING (bucket)
        ),
        diffs AS (
            SELECT *,
                   raw_total - rollup_total AS total_delta,
                   raw_errors - rollup_errors AS errors_delta,
                   raw_captured_bytes - rollup_captured_bytes AS captured_bytes_delta,
                   raw_duration_count - rollup_duration_count AS duration_count_delta,
                   raw_duration_sum_ms - rollup_duration_sum_ms AS duration_sum_ms_delta,
                   raw_ttft_count - rollup_ttft_count AS ttft_count_delta,
                   raw_ttft_sum_ms - rollup_ttft_sum_ms AS ttft_sum_ms_delta,
                   raw_total IS DISTINCT FROM rollup_total
                       OR raw_errors IS DISTINCT FROM rollup_errors
                       OR raw_captured_bytes IS DISTINCT FROM rollup_captured_bytes
                       OR raw_duration_count IS DISTINCT FROM rollup_duration_count
                       OR raw_duration_sum_ms IS DISTINCT FROM rollup_duration_sum_ms
                       OR raw_ttft_count IS DISTINCT FROM rollup_ttft_count
                       OR raw_ttft_sum_ms IS DISTINCT FROM rollup_ttft_sum_ms AS mismatched
            FROM combined
        )
        "#,
            $select
        )
    };
}

pub(super) const DATA_INTEGRITY_ROLLUP_SUMMARY_SQL: &str = data_integrity_rollup_diffs_sql!(
    r#"
        SELECT (SELECT start_bucket FROM bounds) AS start_bucket,
               (SELECT end_bucket FROM bounds) AS end_bucket,
               COUNT(*)::bigint AS compared_bucket_count,
               COUNT(*) FILTER (WHERE mismatched)::bigint AS mismatched_bucket_count,
               COUNT(*) FILTER (WHERE has_raw AND NOT has_rollup)::bigint AS missing_rollup_bucket_count,
               COUNT(*) FILTER (WHERE has_rollup AND NOT has_raw)::bigint AS extra_rollup_bucket_count,
               COALESCE(SUM(raw_total), 0)::bigint AS raw_total,
               COALESCE(SUM(rollup_total), 0)::bigint AS rollup_total,
               COALESCE(SUM(total_delta), 0)::bigint AS total_delta,
               COALESCE(SUM(raw_errors), 0)::bigint AS raw_errors,
               COALESCE(SUM(rollup_errors), 0)::bigint AS rollup_errors,
               COALESCE(SUM(errors_delta), 0)::bigint AS errors_delta,
               COALESCE(SUM(raw_captured_bytes), 0)::bigint AS raw_captured_bytes,
               COALESCE(SUM(rollup_captured_bytes), 0)::bigint AS rollup_captured_bytes,
               COALESCE(SUM(captured_bytes_delta), 0)::bigint AS captured_bytes_delta,
               COALESCE(SUM(raw_duration_count), 0)::bigint AS raw_duration_count,
               COALESCE(SUM(rollup_duration_count), 0)::bigint AS rollup_duration_count,
               COALESCE(SUM(duration_count_delta), 0)::bigint AS duration_count_delta,
               COALESCE(SUM(raw_duration_sum_ms), 0)::bigint AS raw_duration_sum_ms,
               COALESCE(SUM(rollup_duration_sum_ms), 0)::bigint AS rollup_duration_sum_ms,
               COALESCE(SUM(duration_sum_ms_delta), 0)::bigint AS duration_sum_ms_delta,
               COALESCE(SUM(raw_ttft_count), 0)::bigint AS raw_ttft_count,
               COALESCE(SUM(rollup_ttft_count), 0)::bigint AS rollup_ttft_count,
               COALESCE(SUM(ttft_count_delta), 0)::bigint AS ttft_count_delta,
               COALESCE(SUM(raw_ttft_sum_ms), 0)::bigint AS raw_ttft_sum_ms,
               COALESCE(SUM(rollup_ttft_sum_ms), 0)::bigint AS rollup_ttft_sum_ms,
               COALESCE(SUM(ttft_sum_ms_delta), 0)::bigint AS ttft_sum_ms_delta,
               MIN(bucket) FILTER (WHERE mismatched) AS first_mismatch_bucket,
               MAX(bucket) FILTER (WHERE mismatched) AS last_mismatch_bucket
        FROM diffs
        "#
);
pub(super) const DATA_INTEGRITY_ROLLUP_MISMATCHES_SQL: &str = data_integrity_rollup_diffs_sql!(
    r#"
        SELECT bucket,
               has_raw,
               has_rollup,
               raw_total,
               rollup_total,
               total_delta,
               raw_errors,
               rollup_errors,
               errors_delta,
               raw_captured_bytes,
               rollup_captured_bytes,
               captured_bytes_delta,
               raw_duration_count,
               rollup_duration_count,
               duration_count_delta,
               raw_duration_sum_ms,
               rollup_duration_sum_ms,
               duration_sum_ms_delta,
               raw_ttft_count,
               rollup_ttft_count,
               ttft_count_delta,
               raw_ttft_sum_ms,
               rollup_ttft_sum_ms,
               ttft_sum_ms_delta
        FROM diffs
        WHERE mismatched
        ORDER BY bucket DESC
        LIMIT $3
        "#
);
pub(super) const DATA_INTEGRITY_RELATIONSHIP_SQL: &str = r#"
        WITH bounds AS (
            SELECT date_trunc('minute', $1::timestamptz) AS start_bucket
        )
        SELECT (
                   SELECT COUNT(*)::bigint
                   FROM request_traces r, bounds
                   WHERE r.started_at >= bounds.start_bucket
                     AND r.started_at <= $2
                     AND r.session_id IS NOT NULL
                     AND NOT EXISTS (
                         SELECT 1
                         FROM trace_sessions s
                         WHERE s.id = r.session_id
                     )
               ) AS request_missing_session_count,
               (
                   SELECT COUNT(*)::bigint
                   FROM trace_sessions s, bounds
                   WHERE s.last_seen >= bounds.start_bucket
                     AND s.last_seen <= $2
                     AND NOT EXISTS (
                         SELECT 1
                         FROM request_traces r
                         WHERE r.session_id = s.id
                     )
               ) AS empty_session_count,
               (
                   SELECT COUNT(*)::bigint
                   FROM session_messages m, bounds
                   WHERE m.created_at >= bounds.start_bucket
                     AND m.created_at <= $2
                     AND NOT EXISTS (
                         SELECT 1
                         FROM request_traces r
                         WHERE r.id = m.request_id
                     )
               ) AS message_missing_request_count,
               (
                   SELECT COUNT(*)::bigint
                   FROM session_messages m, bounds
                   WHERE m.created_at >= bounds.start_bucket
                     AND m.created_at <= $2
                     AND NOT EXISTS (
                         SELECT 1
                         FROM trace_sessions s
                         WHERE s.id = m.session_id
                     )
               ) AS message_missing_session_count
        "#;
pub async fn usage_summary(
    pool: &PgPool,
    since_hours: Option<i64>,
    limit: Option<i64>,
) -> anyhow::Result<Value> {
    let window = TOP_N_WINDOW.resolve(since_hours, limit);
    let cutoff = window.cutoff(Utc::now());
    let mut tx = begin_api_read_tx(pool).await?;

    let totals = sqlx::query(
        r#"
        SELECT COUNT(*)::bigint AS request_count,
               COUNT(*) FILTER (WHERE error IS NOT NULL OR status >= 400)::bigint AS error_count,
               COALESCE(SUM(bytes_in), 0)::bigint AS bytes_in,
               COALESCE(SUM(bytes_out), 0)::bigint AS bytes_out,
               COALESCE(SUM(request_body_bytes + response_body_bytes), 0)::bigint AS captured_bytes,
               AVG(duration_ms)::bigint AS avg_duration_ms,
               AVG(ttft_ms)::bigint AS avg_ttft_ms
        FROM trace_requests
        WHERE started_at >= $1
        "#,
    )
    .bind(cutoff)
    .fetch_one(&mut *tx)
    .await?;

    let status_classes = sqlx::query(&format!(
        r#"
        SELECT {STATUS_CLASS_SQL} AS name,
               COUNT(*)::bigint AS request_count
        FROM trace_requests
        WHERE started_at >= $1
        GROUP BY 1
        ORDER BY request_count DESC, name ASC
        "#
    ))
    .bind(cutoff)
    .fetch_all(&mut *tx)
    .await?;

    let mut result = breakdowns::<NamedMetric>(
        &mut tx,
        Breakdown::Usage,
        cutoff,
        window.limit,
        &[
            ("top_models", GroupBy::Model),
            ("top_upstreams", GroupBy::Upstream),
            ("request_kinds", GroupBy::RawRequestKind),
        ],
    )
    .await?;
    tx.commit().await?;

    result.insert("window".into(), window.metadata(cutoff));
    result.insert(
        "totals".into(),
        json!({
            "request_count": totals.get::<i64, _>("request_count"),
            "error_count": totals.get::<i64, _>("error_count"),
            "bytes_in": totals.get::<i64, _>("bytes_in"),
            "bytes_out": totals.get::<i64, _>("bytes_out"),
            "captured_bytes": totals.get::<i64, _>("captured_bytes"),
            "avg_duration_ms": totals.try_get::<Option<i64>, _>("avg_duration_ms")?,
            "avg_ttft_ms": totals.try_get::<Option<i64>, _>("avg_ttft_ms")?,
        }),
    );
    result.insert(
        "status_classes".into(),
        serde_json::to_value(read_rows::<NamedCount>(status_classes)?)?,
    );
    Ok(Value::Object(result))
}

pub async fn error_summary(
    pool: &PgPool,
    since_hours: Option<i64>,
    limit: Option<i64>,
) -> anyhow::Result<Value> {
    let window = TOP_N_WINDOW.resolve(since_hours, limit);
    let cutoff = window.cutoff(Utc::now());
    let mut tx = begin_api_read_tx(pool).await?;

    let totals = sqlx::query(ERROR_SUMMARY_TOTALS_SQL)
        .bind(cutoff)
        .fetch_one(&mut *tx)
        .await?;

    let mut result = breakdowns::<ErrorMetric>(
        &mut tx,
        Breakdown::Errors,
        cutoff,
        window.limit,
        &[
            ("sources", GroupBy::ErrorSource),
            ("top_upstreams", GroupBy::Upstream),
            ("top_models", GroupBy::Model),
            ("status_classes", GroupBy::StatusClass),
            ("request_kinds", GroupBy::RequestKind),
        ],
    )
    .await?;
    tx.commit().await?;

    result.insert("window".into(), window.metadata(cutoff));
    result.insert(
        "totals".into(),
        json!({
            "error_count": totals.get::<i64, _>("error_count"),
            "proxy_error_count": totals.get::<i64, _>("proxy_error_count"),
            "http_5xx_count": totals.get::<i64, _>("http_5xx_count"),
            "affected_sessions": totals.get::<i64, _>("affected_sessions"),
            "avg_duration_ms": totals.try_get::<Option<i64>, _>("avg_duration_ms")?,
            "max_duration_ms": totals.try_get::<Option<i64>, _>("max_duration_ms")?,
            "first_seen_at": totals.try_get::<Option<DateTime<Utc>>, _>("first_seen_at")?,
            "last_seen_at": totals.try_get::<Option<DateTime<Utc>>, _>("last_seen_at")?,
        }),
    );
    Ok(Value::Object(result))
}

pub async fn latency_summary(
    pool: &PgPool,
    since_hours: Option<i64>,
    limit: Option<i64>,
) -> anyhow::Result<Value> {
    let window = TOP_N_WINDOW.resolve(since_hours, limit);
    let cutoff = window.cutoff(Utc::now());
    let mut tx = begin_api_read_tx(pool).await?;

    let totals = sqlx::query_as::<_, LatencyMetrics>(&format!(
        "SELECT {LATENCY_COLUMNS} FROM trace_requests WHERE started_at >= $1"
    ))
    .bind(cutoff)
    .fetch_one(&mut *tx)
    .await?;

    let mut result = breakdowns::<NamedLatencyMetrics>(
        &mut tx,
        Breakdown::Latency,
        cutoff,
        window.limit,
        &[
            ("top_upstreams", GroupBy::Upstream),
            ("top_models", GroupBy::Model),
            ("request_kinds", GroupBy::RequestKind),
        ],
    )
    .await?;
    tx.commit().await?;

    result.insert("window".into(), window.metadata(cutoff));
    result.insert("totals".into(), serde_json::to_value(totals)?);
    Ok(Value::Object(result))
}

async fn usage_items<T>(
    pool: &PgPool,
    since_hours: Option<i64>,
    limit: Option<i64>,
    sql: &str,
) -> anyhow::Result<Value>
where
    T: for<'r> FromRow<'r, sqlx::postgres::PgRow> + Serialize + Send + Unpin,
{
    let window = TOP_N_WINDOW.resolve(since_hours, limit);
    let cutoff = window.cutoff(Utc::now());
    let mut tx = begin_api_read_tx(pool).await?;
    let items = sqlx::query_as::<_, T>(sql)
        .bind(cutoff)
        .bind(window.limit)
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(json!({"window": window.metadata(cutoff), "items": items}))
}

pub async fn api_key_usage(
    pool: &PgPool,
    since_hours: Option<i64>,
    limit: Option<i64>,
) -> anyhow::Result<Value> {
    usage_items::<ApiKeyUsage>(pool, since_hours, limit, API_KEY_USAGE_SQL).await
}

pub async fn upstream_health(
    pool: &PgPool,
    since_hours: Option<i64>,
    limit: Option<i64>,
) -> anyhow::Result<Value> {
    usage_items::<UpstreamHealth>(pool, since_hours, limit, UPSTREAM_HEALTH_SQL).await
}

pub async fn model_usage(
    pool: &PgPool,
    since_hours: Option<i64>,
    limit: Option<i64>,
) -> anyhow::Result<Value> {
    usage_items::<ModelUsage>(pool, since_hours, limit, MODEL_USAGE_SQL).await
}

pub async fn user_usage(
    pool: &PgPool,
    since_hours: Option<i64>,
    limit: Option<i64>,
) -> anyhow::Result<Value> {
    usage_items::<UserUsage>(pool, since_hours, limit, USER_USAGE_SQL).await
}

pub async fn usage_timeseries(
    pool: &PgPool,
    since_hours: Option<i64>,
    bucket: Option<&str>,
) -> anyhow::Result<Value> {
    let window = UsageTimeseriesWindow::from_query(since_hours, bucket)?;
    let now = Utc::now();
    let cutoff = window.cutoff(now);
    let mut tx = begin_api_read_tx(pool).await?;
    let mut builder = QueryBuilder::<Postgres>::new("WITH bounds AS (SELECT date_trunc(");
    builder.push_bind(window.bucket.as_str());
    builder.push(", ");
    builder.push_bind(cutoff);
    builder.push("::timestamptz) AS start_bucket, date_trunc(");
    builder.push_bind(window.bucket.as_str());
    builder.push(", ");
    builder.push_bind(now);
    builder.push("::timestamptz) AS end_bucket), series AS (SELECT generate_series(start_bucket, end_bucket, ");
    builder.push(window.bucket.interval_sql());
    builder.push(
        r#") AS bucket FROM bounds), rollups AS (
            SELECT date_trunc("#,
    );
    builder.push_bind(window.bucket.as_str());
    builder.push(
        r#", bucket) AS bucket,
                   COALESCE(SUM(total), 0)::bigint AS request_count,
                   COALESCE(SUM(errors), 0)::bigint AS error_count,
                   COALESCE(SUM(captured_bytes), 0)::bigint AS captured_bytes,
                   COALESCE(SUM(duration_count), 0)::bigint AS duration_count,
                   COALESCE(SUM(duration_sum_ms), 0)::bigint AS duration_sum_ms,
                   COALESCE(SUM(ttft_count), 0)::bigint AS ttft_count,
                   COALESCE(SUM(ttft_sum_ms), 0)::bigint AS ttft_sum_ms
            FROM trace_rollups_minute
            WHERE bucket >= "#,
    );
    builder.push_bind(cutoff);
    builder.push(" AND bucket <= ");
    builder.push_bind(now);
    builder.push(
        r#"
            GROUP BY 1
        )
        SELECT s.bucket,
               COALESCE(r.request_count, 0)::bigint AS request_count,
               COALESCE(r.error_count, 0)::bigint AS error_count,
               COALESCE(r.captured_bytes, 0)::bigint AS captured_bytes,
               CASE
                   WHEN COALESCE(r.duration_count, 0) = 0 THEN NULL
                   ELSE (r.duration_sum_ms / r.duration_count)::bigint
               END AS avg_duration_ms,
               CASE
                   WHEN COALESCE(r.ttft_count, 0) = 0 THEN NULL
                   ELSE (r.ttft_sum_ms / r.ttft_count)::bigint
               END AS avg_ttft_ms
        FROM series s
        LEFT JOIN rollups r ON r.bucket = s.bucket
        ORDER BY s.bucket ASC
        "#,
    );

    let rows = builder.build().fetch_all(&mut *tx).await?;
    tx.commit().await?;

    let points = read_rows::<UsagePoint>(rows)?;

    Ok(json!({
        "window": {
            "since_hours": window.since_hours,
            "started_at_gte": cutoff,
            "bucket": window.bucket.as_str(),
        },
        "points": points,
    }))
}

pub async fn data_overview(pool: &PgPool, since_hours: Option<i64>) -> anyhow::Result<Value> {
    let window = LookbackWindow::from_query(since_hours);
    let now = Utc::now();
    let cutoff = window.cutoff(now);
    let mut tx = begin_api_read_tx(pool).await?;

    let rollups = sqlx::query(DATA_OVERVIEW_ROLLUPS_SQL)
        .fetch_one(&mut *tx)
        .await?;
    let recent = sqlx::query(DATA_OVERVIEW_RECENT_REQUESTS_SQL)
        .bind(cutoff)
        .fetch_one(&mut *tx)
        .await?;
    let sessions = sqlx::query(DATA_OVERVIEW_SESSIONS_SQL)
        .bind(cutoff)
        .fetch_one(&mut *tx)
        .await?;
    let audit = sqlx::query(DATA_OVERVIEW_AUDIT_SQL)
        .bind(cutoff)
        .fetch_one(&mut *tx)
        .await?;
    let auth_state = sqlx::query(DATA_OVERVIEW_AUTH_STATE_SQL)
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;

    tx.commit().await?;

    let request_last_seen_at = rollups.try_get::<Option<DateTime<Utc>>, _>("last_seen_at")?;
    let session_last_seen_at = sessions.try_get::<Option<DateTime<Utc>>, _>("last_seen_at")?;
    let audit_last_seen_at = audit.try_get::<Option<DateTime<Utc>>, _>("last_seen_at")?;
    let recent_request_count = recent.get::<i64, _>("request_count");
    let recent_error_count = recent.get::<i64, _>("error_count");

    Ok(json!({
        "checked_at": now,
        "window": {
            "since_hours": window.since_hours,
            "started_at_gte": cutoff,
        },
        "freshness": {
            "request_last_seen_at": request_last_seen_at,
            "request_last_seen_lag_secs": seconds_since(now, request_last_seen_at),
            "session_last_seen_at": session_last_seen_at,
            "session_last_seen_lag_secs": seconds_since(now, session_last_seen_at),
            "audit_last_seen_at": audit_last_seen_at,
            "audit_last_seen_lag_secs": seconds_since(now, audit_last_seen_at),
        },
        "requests": {
            "totals": {
                "source": "trace_rollups_minute",
                "request_count": rollups.get::<i64, _>("request_count"),
                "error_count": rollups.get::<i64, _>("error_count"),
                "captured_bytes": rollups.get::<i64, _>("captured_bytes"),
                "first_bucket_at": rollups.try_get::<Option<DateTime<Utc>>, _>("first_bucket_at")?,
                "last_bucket_at": rollups.try_get::<Option<DateTime<Utc>>, _>("last_bucket_at")?,
                "last_seen_at": request_last_seen_at,
            },
            "recent": {
                "request_count": recent_request_count,
                "error_count": recent_error_count,
                "error_rate": rate(recent_error_count, recent_request_count),
                "proxy_error_count": recent.get::<i64, _>("proxy_error_count"),
                "http_5xx_count": recent.get::<i64, _>("http_5xx_count"),
                "upstream_count": recent.get::<i64, _>("upstream_count"),
                "model_count": recent.get::<i64, _>("model_count"),
                "session_count": recent.get::<i64, _>("session_count"),
                "bytes_in": recent.get::<i64, _>("bytes_in"),
                "bytes_out": recent.get::<i64, _>("bytes_out"),
                "captured_bytes": recent.get::<i64, _>("captured_bytes"),
                "avg_duration_ms": recent.try_get::<Option<i64>, _>("avg_duration_ms")?,
                "max_duration_ms": recent.try_get::<Option<i64>, _>("max_duration_ms")?,
                "avg_ttft_ms": recent.try_get::<Option<i64>, _>("avg_ttft_ms")?,
                "max_ttft_ms": recent.try_get::<Option<i64>, _>("max_ttft_ms")?,
                "first_seen_at": recent.try_get::<Option<DateTime<Utc>>, _>("first_seen_at")?,
                "last_seen_at": recent.try_get::<Option<DateTime<Utc>>, _>("last_seen_at")?,
            },
        },
        "sessions": {
            "session_count": sessions.get::<i64, _>("session_count"),
            "active_session_count": sessions.get::<i64, _>("active_session_count"),
            "first_seen_at": sessions.try_get::<Option<DateTime<Utc>>, _>("first_seen_at")?,
            "last_seen_at": session_last_seen_at,
        },
        "audit": {
            "event_count": audit.get::<i64, _>("event_count"),
            "recent_event_count": audit.get::<i64, _>("recent_event_count"),
            "event_type_count": audit.get::<i64, _>("event_type_count"),
            "first_seen_at": audit.try_get::<Option<DateTime<Utc>>, _>("first_seen_at")?,
            "last_seen_at": audit_last_seen_at,
        },
        "auth_state": {
            "active_ui_sessions": auth_state.get::<i64, _>("active_ui_sessions"),
            "expired_ui_sessions": auth_state.get::<i64, _>("expired_ui_sessions"),
            "pending_oauth_states": auth_state.get::<i64, _>("pending_oauth_states"),
            "expired_oauth_states": auth_state.get::<i64, _>("expired_oauth_states"),
        },
    }))
}

pub async fn data_integrity(pool: &PgPool, since_hours: Option<i64>) -> anyhow::Result<Value> {
    let window = LookbackWindow::from_query(since_hours);
    let now = Utc::now();
    let cutoff = window.cutoff(now);
    let mut tx = begin_api_read_tx(pool).await?;

    let rollup_summary = sqlx::query(DATA_INTEGRITY_ROLLUP_SUMMARY_SQL)
        .bind(cutoff)
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;
    let rollup_mismatches = sqlx::query(DATA_INTEGRITY_ROLLUP_MISMATCHES_SQL)
        .bind(cutoff)
        .bind(now)
        .bind(DATA_INTEGRITY_MISMATCH_LIMIT)
        .fetch_all(&mut *tx)
        .await?;
    let relationships = sqlx::query(DATA_INTEGRITY_RELATIONSHIP_SQL)
        .bind(cutoff)
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;

    tx.commit().await?;

    let mismatched_bucket_count = rollup_summary.get::<i64, _>("mismatched_bucket_count");
    let request_missing_session_count =
        relationships.get::<i64, _>("request_missing_session_count");
    let message_missing_request_count =
        relationships.get::<i64, _>("message_missing_request_count");
    let message_missing_session_count =
        relationships.get::<i64, _>("message_missing_session_count");
    let rollups_consistent = mismatched_bucket_count == 0;
    let references_consistent = request_missing_session_count == 0
        && message_missing_request_count == 0
        && message_missing_session_count == 0;
    let status = if rollups_consistent && references_consistent {
        "ok"
    } else {
        "attention"
    };

    Ok(json!({
        "checked_at": now,
        "status": status,
        "window": {
            "since_hours": window.since_hours,
            "started_at_gte": cutoff,
            "bucket_started_at_gte": rollup_summary.get::<DateTime<Utc>, _>("start_bucket"),
            "bucket_started_at_lte": rollup_summary.get::<DateTime<Utc>, _>("end_bucket"),
            "mismatch_limit": DATA_INTEGRITY_MISMATCH_LIMIT,
        },
        "rollups": {
            "consistent": rollups_consistent,
            "compared_bucket_count": rollup_summary.get::<i64, _>("compared_bucket_count"),
            "mismatched_bucket_count": mismatched_bucket_count,
            "missing_rollup_bucket_count": rollup_summary.get::<i64, _>("missing_rollup_bucket_count"),
            "extra_rollup_bucket_count": rollup_summary.get::<i64, _>("extra_rollup_bucket_count"),
            "first_mismatch_bucket": rollup_summary.try_get::<Option<DateTime<Utc>>, _>("first_mismatch_bucket")?,
            "last_mismatch_bucket": rollup_summary.try_get::<Option<DateTime<Utc>>, _>("last_mismatch_bucket")?,
            "metrics": rollup_integrity_metrics(&rollup_summary),
            "mismatches": rollup_mismatches
                .into_iter()
                .map(rollup_integrity_mismatch_row)
                .collect::<Vec<_>>(),
        },
        "relationships": {
            "consistent": references_consistent,
            "request_missing_session_count": request_missing_session_count,
            "message_missing_request_count": message_missing_request_count,
            "message_missing_session_count": message_missing_session_count,
            "empty_session_count": relationships.get::<i64, _>("empty_session_count"),
        },
    }))
}

pub(super) fn rollup_integrity_metrics(row: &sqlx::postgres::PgRow) -> Value {
    json!({
        "requests": rollup_integrity_metric(row, "raw_total", "rollup_total", "total_delta"),
        "errors": rollup_integrity_metric(row, "raw_errors", "rollup_errors", "errors_delta"),
        "captured_bytes": rollup_integrity_metric(
            row,
            "raw_captured_bytes",
            "rollup_captured_bytes",
            "captured_bytes_delta",
        ),
        "duration_count": rollup_integrity_metric(
            row,
            "raw_duration_count",
            "rollup_duration_count",
            "duration_count_delta",
        ),
        "duration_sum_ms": rollup_integrity_metric(
            row,
            "raw_duration_sum_ms",
            "rollup_duration_sum_ms",
            "duration_sum_ms_delta",
        ),
        "ttft_count": rollup_integrity_metric(
            row,
            "raw_ttft_count",
            "rollup_ttft_count",
            "ttft_count_delta",
        ),
        "ttft_sum_ms": rollup_integrity_metric(
            row,
            "raw_ttft_sum_ms",
            "rollup_ttft_sum_ms",
            "ttft_sum_ms_delta",
        ),
    })
}

pub(super) fn rollup_integrity_mismatch_row(row: sqlx::postgres::PgRow) -> Value {
    json!({
        "bucket": row.get::<DateTime<Utc>, _>("bucket"),
        "has_raw": row.get::<bool, _>("has_raw"),
        "has_rollup": row.get::<bool, _>("has_rollup"),
        "metrics": rollup_integrity_metrics(&row),
    })
}

pub(super) fn rollup_integrity_metric(
    row: &sqlx::postgres::PgRow,
    raw_column: &str,
    rollup_column: &str,
    delta_column: &str,
) -> Value {
    json!({
        "raw": row.get::<i64, _>(raw_column),
        "rollup": row.get::<i64, _>(rollup_column),
        "delta": row.get::<i64, _>(delta_column),
    })
}

pub async fn stats(pool: &PgPool) -> anyhow::Result<Value> {
    let mut tx = begin_api_read_tx(pool).await?;
    let row = sqlx::query(
        r#"
        SELECT
          COALESCE(SUM(total), 0)::bigint AS total,
          COALESCE(SUM(total) FILTER (WHERE bucket >= date_trunc('minute', now() - interval '1 hour')), 0)::bigint AS last_hour,
          COALESCE(SUM(errors), 0)::bigint AS errors,
          COALESCE(SUM(captured_bytes), 0)::bigint AS captured_bytes,
          COALESCE(SUM(duration_count), 0)::bigint AS duration_count,
          COALESCE(SUM(duration_sum_ms), 0)::bigint AS duration_sum_ms,
          COALESCE(SUM(ttft_count), 0)::bigint AS ttft_count,
          COALESCE(SUM(ttft_sum_ms), 0)::bigint AS ttft_sum_ms
        FROM trace_rollups_minute
        "#,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    let duration_count = row.get::<i64, _>("duration_count");
    let ttft_count = row.get::<i64, _>("ttft_count");
    let avg_duration_ms = if duration_count == 0 {
        None
    } else {
        Some(row.get::<i64, _>("duration_sum_ms") / duration_count)
    };
    let avg_ttft_ms = if ttft_count == 0 {
        None
    } else {
        Some(row.get::<i64, _>("ttft_sum_ms") / ttft_count)
    };

    Ok(json!({
        "total": row.get::<i64, _>("total"),
        "last_hour": row.get::<i64, _>("last_hour"),
        "errors": row.get::<i64, _>("errors"),
        "captured_bytes": row.get::<i64, _>("captured_bytes"),
        "avg_duration_ms": avg_duration_ms,
        "avg_ttft_ms": avg_ttft_ms,
    }))
}
