use std::future::Future;
use std::io::Read;
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Postgres, QueryBuilder, Row, Transaction};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::config::StorageConfig;
use crate::metrics::RuntimeMetrics;
use crate::types::RequestKind;

const DEFAULT_REQUEST_LIST_LIMIT: i64 = 100;
const MAX_REQUEST_LIST_LIMIT: i64 = 500;
const MAX_REQUEST_LIST_OFFSET: i64 = 1_000_000;
const DEFAULT_REQUEST_FACET_SINCE_HOURS: i64 = 24;
const MAX_REQUEST_FACET_SINCE_HOURS: i64 = 24 * 90;
const DEFAULT_REQUEST_FACET_LIMIT: i64 = 25;
const MAX_REQUEST_FACET_LIMIT: i64 = 100;
const DEFAULT_RECENT_ERROR_SINCE_HOURS: i64 = 24;
const MAX_RECENT_ERROR_SINCE_HOURS: i64 = 24 * 90;
const DEFAULT_RECENT_ERROR_LIMIT: i64 = 50;
const MAX_RECENT_ERROR_LIMIT: i64 = 200;
const DEFAULT_SESSION_LIST_LIMIT: i64 = 100;
const MAX_SESSION_LIST_LIMIT: i64 = 500;
const MAX_SESSION_LIST_OFFSET: i64 = 1_000_000;
const DEFAULT_SESSION_MESSAGE_LIMIT: i64 = 100;
const MAX_SESSION_MESSAGE_LIMIT: i64 = 500;
const MAX_SESSION_MESSAGE_OFFSET: i64 = 1_000_000;
const DEFAULT_AUDIT_EVENT_LIMIT: i64 = 100;
const MAX_AUDIT_EVENT_LIMIT: i64 = 500;
const MAX_AUDIT_EVENT_OFFSET: i64 = 1_000_000;
const DEFAULT_USAGE_SUMMARY_SINCE_HOURS: i64 = 24;
const MAX_USAGE_SUMMARY_SINCE_HOURS: i64 = 24 * 90;
const DEFAULT_USAGE_SUMMARY_LIMIT: i64 = 10;
const MAX_USAGE_SUMMARY_LIMIT: i64 = 50;
const DEFAULT_USAGE_TIMESERIES_SINCE_HOURS: i64 = 24;
const MAX_USAGE_TIMESERIES_MINUTE_HOURS: i64 = 24;
const MAX_USAGE_TIMESERIES_HOUR_HOURS: i64 = 24 * 90;
const MAX_USAGE_TIMESERIES_DAY_HOURS: i64 = 24 * 365;
const MAX_STRUCTURED_QUERY_FIELDS: usize = 64;
const MAX_STRUCTURED_QUERY_FILTERS: usize = 32;
const MAX_STRUCTURED_QUERY_ORDER_BY: usize = 8;
const MAX_STRUCTURED_QUERY_STRING_VALUE_BYTES: usize = 4 * 1024;
const MAX_STRUCTURED_QUERY_JSON_VALUE_BYTES: usize = 16 * 1024;
const API_READ_STATEMENT_TIMEOUT_SQL: &str = "SET LOCAL statement_timeout = '5s'";
const READINESS_CHECK_TIMEOUT: Duration = Duration::from_secs(2);
const REQUEST_FACET_MODELS_SQL: &str = r#"
        SELECT model AS value, COUNT(*)::bigint AS request_count
        FROM trace_requests
        WHERE started_at >= $1
          AND model IS NOT NULL
          AND model <> ''
        GROUP BY model
        ORDER BY request_count DESC, value ASC
        LIMIT $2
        "#;
const REQUEST_FACET_UPSTREAM_HOSTS_SQL: &str = r#"
        SELECT upstream_host AS value, COUNT(*)::bigint AS request_count
        FROM trace_requests
        WHERE started_at >= $1
          AND upstream_host IS NOT NULL
          AND upstream_host <> ''
        GROUP BY upstream_host
        ORDER BY request_count DESC, value ASC
        LIMIT $2
        "#;
const REQUEST_FACET_REQUEST_KINDS_SQL: &str = r#"
        SELECT request_kind AS value, COUNT(*)::bigint AS request_count
        FROM trace_requests
        WHERE started_at >= $1
          AND request_kind IS NOT NULL
          AND request_kind <> ''
        GROUP BY request_kind
        ORDER BY request_count DESC, value ASC
        LIMIT $2
        "#;
const REQUEST_FACET_STATUSES_SQL: &str = r#"
        SELECT status AS value, COUNT(*)::bigint AS request_count
        FROM trace_requests
        WHERE started_at >= $1
          AND status IS NOT NULL
        GROUP BY status
        ORDER BY request_count DESC, value ASC
        LIMIT $2
        "#;
const REQUEST_FACET_STATUS_CLASSES_SQL: &str = r#"
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
const REQUEST_FACET_ERROR_STATES_SQL: &str = r#"
        SELECT (error IS NOT NULL OR COALESCE(status >= 500, false)) AS value,
               COUNT(*)::bigint AS request_count
        FROM trace_requests
        WHERE started_at >= $1
        GROUP BY 1
        ORDER BY value DESC
        "#;
const LIST_REQUESTS_SQL: &str = r#"
        SELECT id, started_at, completed_at, method, original_uri, upstream_url, upstream_host,
               status, error, request_kind, model, api_key_hash, session_id, ttft_ms,
               duration_ms, bytes_in, bytes_out, request_body_truncated, response_body_truncated,
               plugin_metadata, tags
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
          AND ($7::timestamptz IS NULL OR started_at >= $7)
          AND ($8::timestamptz IS NULL OR started_at <= $8)
          AND ($9::bigint IS NULL OR duration_ms >= $9)
          AND ($10::bigint IS NULL OR duration_ms <= $10)
          AND (
              $11::boolean IS NULL
              OR ($11 = true AND (error IS NOT NULL OR status >= 500))
              OR ($11 = false AND error IS NULL AND (status IS NULL OR status < 500))
          )
          AND (
              $12::text IS NULL
              OR ($12 = 'no_status' AND status IS NULL)
              OR ($12 = '1xx' AND status BETWEEN 100 AND 199)
              OR ($12 = '2xx' AND status BETWEEN 200 AND 299)
              OR ($12 = '3xx' AND status BETWEEN 300 AND 399)
              OR ($12 = '4xx' AND status BETWEEN 400 AND 499)
              OR ($12 = '5xx' AND status BETWEEN 500 AND 599)
              OR ($12 = 'other' AND status IS NOT NULL AND (status < 100 OR status > 599))
          )
        ORDER BY started_at DESC, id DESC
        LIMIT $13 OFFSET $14
        "#;
const LIST_SESSION_REQUESTS_SQL: &str = r#"
        SELECT id, started_at, completed_at, method, original_uri, upstream_url, upstream_host,
               status, error, request_kind, model, api_key_hash, session_id, ttft_ms,
               duration_ms, bytes_in, bytes_out, request_body_truncated, response_body_truncated,
               plugin_metadata, tags
        FROM trace_requests
        WHERE session_id = $1
        ORDER BY started_at DESC, id DESC
        LIMIT $2 OFFSET $3
        "#;
const RECENT_ERROR_REQUESTS_SQL: &str = r#"
        SELECT id, started_at, completed_at, method, original_uri, upstream_url, upstream_host,
               status, error, request_kind, model, api_key_hash, session_id, ttft_ms,
               duration_ms, bytes_in, bytes_out, request_body_truncated, response_body_truncated,
               plugin_metadata, tags
        FROM trace_requests
        WHERE started_at >= $1
          AND (error IS NOT NULL OR status >= 500)
        ORDER BY started_at DESC, id DESC
        LIMIT $2
        "#;
const SESSION_REQUEST_STATS_SQL: &str = r#"
        SELECT COUNT(*)::bigint AS request_count,
               COUNT(*) FILTER (WHERE error IS NOT NULL OR status >= 500)::bigint AS error_count,
               COALESCE(SUM(bytes_in), 0)::bigint AS bytes_in,
               COALESCE(SUM(bytes_out), 0)::bigint AS bytes_out,
               COALESCE(SUM(request_body_bytes + response_body_bytes), 0)::bigint AS captured_bytes,
               AVG(duration_ms)::bigint AS avg_duration_ms,
               MAX(duration_ms)::bigint AS max_duration_ms,
               AVG(ttft_ms)::bigint AS avg_ttft_ms,
               MAX(ttft_ms)::bigint AS max_ttft_ms,
               MIN(started_at) AS first_request_at,
               MAX(started_at) AS last_request_at
        FROM request_traces
        WHERE session_id = $1
        "#;
const LIST_SESSIONS_SQL: &str = r#"
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetentionPruneResult {
    pub request_traces: u64,
    pub trace_rollups_minute: u64,
    pub trace_sessions: u64,
    pub ui_audit_events: u64,
    pub ui_sessions: u64,
    pub oauth_states: u64,
}

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
    pub duration_ms: Option<i64>,
    pub bytes_in: i64,
    pub bytes_out: i64,
    pub request_headers: Value,
    pub response_headers: Value,
    pub request_body_compressed: Vec<u8>,
    pub response_body_compressed: Vec<u8>,
    pub request_body_bytes: i64,
    pub response_body_bytes: i64,
    pub request_body_truncated: bool,
    pub response_body_truncated: bool,
    pub content_type: Option<String>,
    pub plugin_metadata: Value,
    pub tags: Vec<String>,
}

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
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub min_duration_ms: Option<i64>,
    pub max_duration_ms: Option<i64>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ParsedMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StructuredQuery {
    pub dataset: String,
    pub fields: Option<Vec<String>>,
    #[serde(default)]
    pub filters: Vec<QueryFilter>,
    #[serde(default)]
    pub order_by: Vec<QueryOrder>,
    pub limit: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum StructuredQueryError {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Execution(#[from] anyhow::Error),
}

impl StructuredQueryError {
    fn invalid(error: impl std::fmt::Display) -> Self {
        Self::Invalid(error.to_string())
    }

    fn execution(error: impl Into<anyhow::Error>) -> Self {
        Self::Execution(error.into())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct QueryFilter {
    pub field: String,
    pub op: QueryOp,
    pub value: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QueryOrder {
    pub field: String,
    #[serde(default)]
    pub direction: SortDirection,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryOp {
    Eq,
    Ne,
    Contains,
    Gt,
    Gte,
    Lt,
    Lte,
    IsNull,
    IsNotNull,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDirection {
    Asc,
    #[default]
    Desc,
}

impl SortDirection {
    fn as_str(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct DatasetSpec {
    name: &'static str,
    relation: &'static str,
    fields: &'static [FieldSpec],
    default_fields: &'static [&'static str],
    default_order: &'static [DefaultOrder],
}

#[derive(Debug, Clone, Copy)]
struct FieldSpec {
    name: &'static str,
    sql: &'static str,
    filter: Option<FilterKind>,
}

#[derive(Debug, Clone)]
enum QueryField {
    Static(&'static FieldSpec),
    PluginMetadataPath { name: String, path: Vec<String> },
}

#[derive(Debug, Clone, Copy)]
struct DefaultOrder {
    field: &'static str,
    direction: SortDirection,
}

#[derive(Debug, Clone, Copy)]
enum FilterKind {
    Text,
    Int,
    Bool,
    Timestamp,
    Uuid,
    Json,
    JsonPath,
    TextArray,
}

impl FilterKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Int => "int",
            Self::Bool => "bool",
            Self::Timestamp => "timestamp",
            Self::Uuid => "uuid",
            Self::Json => "json",
            Self::JsonPath => "json_path",
            Self::TextArray => "text_array",
        }
    }

    fn operators(self) -> &'static [&'static str] {
        match self {
            Self::Text => &[
                "eq",
                "ne",
                "contains",
                "gt",
                "gte",
                "lt",
                "lte",
                "is_null",
                "is_not_null",
            ],
            Self::Int | Self::Timestamp => &[
                "eq",
                "ne",
                "gt",
                "gte",
                "lt",
                "lte",
                "is_null",
                "is_not_null",
            ],
            Self::Bool | Self::Uuid => &["eq", "ne", "is_null", "is_not_null"],
            Self::Json | Self::JsonPath => &["eq", "ne", "contains", "is_null", "is_not_null"],
            Self::TextArray => &["contains", "is_null", "is_not_null"],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RequestListPage {
    limit: i64,
    offset: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RequestFacetWindow {
    since_hours: i64,
    limit: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RecentErrorWindow {
    since_hours: i64,
    limit: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SessionListPage {
    limit: i64,
    offset: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SessionMessagePage {
    limit: i64,
    offset: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AuditEventPage {
    limit: i64,
    offset: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UsageSummaryWindow {
    since_hours: i64,
    limit: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsageTimeseriesBucket {
    Minute,
    Hour,
    Day,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UsageTimeseriesWindow {
    since_hours: i64,
    bucket: UsageTimeseriesBucket,
}

impl QueryField {
    fn name(&self) -> &str {
        match self {
            Self::Static(field) => field.name,
            Self::PluginMetadataPath { name, .. } => name,
        }
    }

    fn filter(&self) -> Option<FilterKind> {
        match self {
            Self::Static(field) => field.filter,
            Self::PluginMetadataPath { .. } => Some(FilterKind::JsonPath),
        }
    }

    fn append_sql(&self, builder: &mut QueryBuilder<'_, Postgres>) {
        match self {
            Self::Static(field) => {
                builder.push(field.sql);
            }
            Self::PluginMetadataPath { path, .. } => {
                builder.push("(plugin_metadata #> ");
                builder.push_bind(path.clone());
                builder.push(")");
            }
        }
    }
}

impl RequestListPage {
    fn from_query(limit: Option<i64>, offset: Option<i64>) -> Self {
        Self {
            limit: limit
                .unwrap_or(DEFAULT_REQUEST_LIST_LIMIT)
                .clamp(1, MAX_REQUEST_LIST_LIMIT),
            offset: offset.unwrap_or(0).clamp(0, MAX_REQUEST_LIST_OFFSET),
        }
    }

    fn fetch_limit(self) -> i64 {
        self.limit + 1
    }

    fn next_offset(self, has_more: bool) -> Option<i64> {
        has_more.then_some(self.offset.saturating_add(self.limit))
    }
}

impl RequestFacetWindow {
    fn from_query(since_hours: Option<i64>, limit: Option<i64>) -> Self {
        Self {
            since_hours: since_hours
                .unwrap_or(DEFAULT_REQUEST_FACET_SINCE_HOURS)
                .clamp(1, MAX_REQUEST_FACET_SINCE_HOURS),
            limit: limit
                .unwrap_or(DEFAULT_REQUEST_FACET_LIMIT)
                .clamp(1, MAX_REQUEST_FACET_LIMIT),
        }
    }

    fn cutoff(self, now: DateTime<Utc>) -> DateTime<Utc> {
        now - ChronoDuration::hours(self.since_hours)
    }
}

impl RecentErrorWindow {
    fn from_query(since_hours: Option<i64>, limit: Option<i64>) -> Self {
        Self {
            since_hours: since_hours
                .unwrap_or(DEFAULT_RECENT_ERROR_SINCE_HOURS)
                .clamp(1, MAX_RECENT_ERROR_SINCE_HOURS),
            limit: limit
                .unwrap_or(DEFAULT_RECENT_ERROR_LIMIT)
                .clamp(1, MAX_RECENT_ERROR_LIMIT),
        }
    }

    fn cutoff(self, now: DateTime<Utc>) -> DateTime<Utc> {
        now - ChronoDuration::hours(self.since_hours)
    }

    fn fetch_limit(self) -> i64 {
        self.limit + 1
    }
}

impl SessionListPage {
    fn from_query(limit: Option<i64>, offset: Option<i64>) -> Self {
        Self {
            limit: limit
                .unwrap_or(DEFAULT_SESSION_LIST_LIMIT)
                .clamp(1, MAX_SESSION_LIST_LIMIT),
            offset: offset.unwrap_or(0).clamp(0, MAX_SESSION_LIST_OFFSET),
        }
    }

    fn fetch_limit(self) -> i64 {
        self.limit + 1
    }

    fn next_offset(self, has_more: bool) -> Option<i64> {
        has_more.then_some(self.offset.saturating_add(self.limit))
    }
}

impl SessionMessagePage {
    fn from_query(limit: Option<i64>, offset: Option<i64>) -> Self {
        Self {
            limit: limit
                .unwrap_or(DEFAULT_SESSION_MESSAGE_LIMIT)
                .clamp(1, MAX_SESSION_MESSAGE_LIMIT),
            offset: offset.unwrap_or(0).clamp(0, MAX_SESSION_MESSAGE_OFFSET),
        }
    }

    fn fetch_limit(self) -> i64 {
        self.limit + 1
    }

    fn next_offset(self, has_more: bool) -> Option<i64> {
        has_more.then_some(self.offset.saturating_add(self.limit))
    }
}

impl AuditEventPage {
    fn from_query(limit: Option<i64>, offset: Option<i64>) -> Self {
        Self {
            limit: limit
                .unwrap_or(DEFAULT_AUDIT_EVENT_LIMIT)
                .clamp(1, MAX_AUDIT_EVENT_LIMIT),
            offset: offset.unwrap_or(0).clamp(0, MAX_AUDIT_EVENT_OFFSET),
        }
    }

    fn fetch_limit(self) -> i64 {
        self.limit + 1
    }

    fn next_offset(self, has_more: bool) -> Option<i64> {
        has_more.then_some(self.offset.saturating_add(self.limit))
    }
}

impl UsageSummaryWindow {
    fn from_query(since_hours: Option<i64>, limit: Option<i64>) -> Self {
        Self {
            since_hours: since_hours
                .unwrap_or(DEFAULT_USAGE_SUMMARY_SINCE_HOURS)
                .clamp(1, MAX_USAGE_SUMMARY_SINCE_HOURS),
            limit: limit
                .unwrap_or(DEFAULT_USAGE_SUMMARY_LIMIT)
                .clamp(1, MAX_USAGE_SUMMARY_LIMIT),
        }
    }

    fn cutoff(self, now: DateTime<Utc>) -> DateTime<Utc> {
        now - ChronoDuration::hours(self.since_hours)
    }
}

impl UsageTimeseriesBucket {
    fn parse(value: Option<&str>) -> anyhow::Result<Self> {
        let Some(value) = value else {
            return Ok(Self::Hour);
        };
        match value.trim().to_ascii_lowercase().as_str() {
            "" => Ok(Self::Hour),
            "minute" => Ok(Self::Minute),
            "hour" => Ok(Self::Hour),
            "day" => Ok(Self::Day),
            _ => anyhow::bail!("bucket must be one of minute, hour, or day"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Minute => "minute",
            Self::Hour => "hour",
            Self::Day => "day",
        }
    }

    fn interval_sql(self) -> &'static str {
        match self {
            Self::Minute => "'1 minute'::interval",
            Self::Hour => "'1 hour'::interval",
            Self::Day => "'1 day'::interval",
        }
    }

    fn max_since_hours(self) -> i64 {
        match self {
            Self::Minute => MAX_USAGE_TIMESERIES_MINUTE_HOURS,
            Self::Hour => MAX_USAGE_TIMESERIES_HOUR_HOURS,
            Self::Day => MAX_USAGE_TIMESERIES_DAY_HOURS,
        }
    }
}

impl UsageTimeseriesWindow {
    fn from_query(since_hours: Option<i64>, bucket: Option<&str>) -> anyhow::Result<Self> {
        let bucket = UsageTimeseriesBucket::parse(bucket)?;
        Ok(Self {
            since_hours: since_hours
                .unwrap_or(DEFAULT_USAGE_TIMESERIES_SINCE_HOURS)
                .clamp(1, bucket.max_since_hours()),
            bucket,
        })
    }

    fn cutoff(self, now: DateTime<Utc>) -> DateTime<Utc> {
        now - ChronoDuration::hours(self.since_hours)
    }
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

pub async fn connect(config: &StorageConfig) -> anyhow::Result<PgPool> {
    Ok(PgPoolOptions::new()
        .max_connections(config.max_connections)
        .acquire_timeout(Duration::from_secs(config.acquire_timeout_secs))
        .connect(&config.postgres_url)
        .await?)
}

pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}

pub async fn readiness_check(pool: &PgPool) -> anyhow::Result<()> {
    readiness_check_with_timeout(
        async {
            sqlx::query_scalar::<_, i32>("SELECT 1")
                .fetch_one(pool)
                .await?;
            Ok(())
        },
        READINESS_CHECK_TIMEOUT,
    )
    .await
}

async fn readiness_check_with_timeout<F>(
    future: F,
    timeout_duration: Duration,
) -> anyhow::Result<()>
where
    F: Future<Output = anyhow::Result<()>>,
{
    match tokio::time::timeout(timeout_duration, future).await {
        Ok(result) => result,
        Err(_) => anyhow::bail!(
            "readiness check timed out after {} ms",
            timeout_duration.as_millis()
        ),
    }
}

async fn begin_api_read_tx(pool: &PgPool) -> anyhow::Result<Transaction<'_, Postgres>> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION READ ONLY")
        .execute(&mut *tx)
        .await?;
    sqlx::query(API_READ_STATEMENT_TIMEOUT_SQL)
        .execute(&mut *tx)
        .await?;
    Ok(tx)
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

fn retention_cutoff(now: DateTime<Utc>, retention_days: i64) -> DateTime<Utc> {
    now - ChronoDuration::days(retention_days)
}

async fn delete_request_traces_before(
    pool: &PgPool,
    cutoff: DateTime<Utc>,
    batch_size: i64,
) -> anyhow::Result<u64> {
    let result = sqlx::query(
        r#"
        WITH doomed AS (
            SELECT id
            FROM request_traces
            WHERE started_at < $1
            ORDER BY started_at ASC
            LIMIT $2
        )
        DELETE FROM request_traces r
        USING doomed
        WHERE r.id = doomed.id
        "#,
    )
    .bind(cutoff)
    .bind(batch_size)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

async fn delete_rollups_before(
    pool: &PgPool,
    cutoff: DateTime<Utc>,
    batch_size: i64,
) -> anyhow::Result<u64> {
    let result = sqlx::query(
        r#"
        WITH doomed AS (
            SELECT bucket
            FROM trace_rollups_minute
            WHERE bucket < $1
            ORDER BY bucket ASC
            LIMIT $2
        )
        DELETE FROM trace_rollups_minute r
        USING doomed
        WHERE r.bucket = doomed.bucket
        "#,
    )
    .bind(cutoff)
    .bind(batch_size)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

async fn delete_empty_sessions_before(
    pool: &PgPool,
    cutoff: DateTime<Utc>,
    batch_size: i64,
) -> anyhow::Result<u64> {
    let result = sqlx::query(
        r#"
        WITH doomed AS (
            SELECT s.id
            FROM trace_sessions s
            WHERE s.last_seen < $1
              AND NOT EXISTS (
                  SELECT 1
                  FROM request_traces r
                  WHERE r.session_id = s.id
              )
            ORDER BY s.last_seen ASC
            LIMIT $2
        )
        DELETE FROM trace_sessions s
        USING doomed
        WHERE s.id = doomed.id
        "#,
    )
    .bind(cutoff)
    .bind(batch_size)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

async fn delete_ui_audit_events_before(
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

async fn delete_expired_ui_sessions(
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

async fn delete_expired_oauth_states(
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

pub fn compress(data: &[u8]) -> anyhow::Result<Vec<u8>> {
    if data.is_empty() {
        return Ok(Vec::new());
    }
    Ok(zstd::stream::encode_all(data, 3)?)
}

pub fn decompress_with_limit(data: &[u8], limit: usize) -> anyhow::Result<Vec<u8>> {
    if data.is_empty() {
        return Ok(Vec::new());
    }
    let read_limit = limit
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("body decompression limit is too large"))?;
    let mut decoder = zstd::stream::read::Decoder::new(data)?;
    let mut output = Vec::new();
    decoder
        .by_ref()
        .take(read_limit as u64)
        .read_to_end(&mut output)?;
    if output.len() > limit {
        anyhow::bail!("stored trace body exceeds decompression limit of {limit} bytes");
    }
    Ok(output)
}

pub async fn insert_trace(
    pool: &PgPool,
    mut trace: TraceRecord,
    messages: Vec<ParsedMessage>,
    user_id: Option<String>,
    user_name: Option<String>,
) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;

    if trace.session_id.is_none()
        && let Some(session_key) = trace.session_key.clone()
    {
        trace.session_id = Some(upsert_session(&mut tx, &session_key, user_id, user_name).await?);
    }

    sqlx::query(
        r#"
        INSERT INTO request_traces (
            id, started_at, completed_at, method, original_uri, upstream_url, upstream_host,
            status, error, request_kind, model, api_key_hash, session_key, session_id,
            ttft_ms, duration_ms, bytes_in, bytes_out, request_headers, response_headers,
            request_body_compressed, response_body_compressed, request_body_bytes, response_body_bytes,
            request_body_truncated, response_body_truncated, content_type, plugin_metadata, tags
        )
        VALUES (
            $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,
            $21,$22,$23,$24,$25,$26,$27,$28,$29
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
    .bind(&trace.request_body_compressed)
    .bind(&trace.response_body_compressed)
    .bind(trace.request_body_bytes)
    .bind(trace.response_body_bytes)
    .bind(trace.request_body_truncated)
    .bind(trace.response_body_truncated)
    .bind(&trace.content_type)
    .bind(&trace.plugin_metadata)
    .bind(&trace.tags)
    .execute(&mut *tx)
    .await?;

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
            SELECT $1, $2, message.role, message.content, now()
            FROM UNNEST($3::text[], $4::text[]) AS message(role, content)
            "#,
        )
        .bind(trace.id)
        .bind(session_id)
        .bind(&roles)
        .bind(&contents)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

async fn upsert_session(
    tx: &mut Transaction<'_, Postgres>,
    session_key: &str,
    user_id: Option<String>,
    user_name: Option<String>,
) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    let row = sqlx::query(
        r#"
        INSERT INTO trace_sessions (id, session_key, first_seen, last_seen, user_id, user_name, summary)
        VALUES ($1, $2, now(), now(), $3, $4, '{}'::jsonb)
        ON CONFLICT (session_key) DO UPDATE
        SET last_seen = now(),
            user_id = COALESCE(trace_sessions.user_id, EXCLUDED.user_id),
            user_name = COALESCE(trace_sessions.user_name, EXCLUDED.user_name)
        RETURNING id
        "#,
    )
    .bind(id)
    .bind(session_key)
    .bind(user_id)
    .bind(user_name)
    .fetch_one(&mut **tx)
    .await?;

    Ok(row.try_get("id")?)
}

async fn update_rollup(
    tx: &mut Transaction<'_, Postgres>,
    trace: &TraceRecord,
) -> anyhow::Result<()> {
    let errors = if trace.error.is_some() || trace.status.is_some_and(|status| status >= 500) {
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

pub async fn list_requests(pool: &PgPool, filters: RequestListFilters) -> anyhow::Result<Value> {
    let q = filters.q.map(|value| escape_like(&value));
    let page = RequestListPage::from_query(filters.limit, filters.offset);
    let mut tx = begin_api_read_tx(pool).await?;
    let rows = sqlx::query(LIST_REQUESTS_SQL)
        .bind(q)
        .bind(filters.status)
        .bind(filters.upstream_host)
        .bind(filters.model)
        .bind(filters.request_kind)
        .bind(filters.session_id)
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
    let items: Vec<Value> = rows
        .into_iter()
        .take(page.limit as usize)
        .map(request_summary_row)
        .collect();
    Ok(json!({
        "items": items,
        "page": {
            "limit": page.limit,
            "offset": page.offset,
            "has_more": has_more,
            "next_offset": page.next_offset(has_more),
        },
    }))
}

pub async fn list_session_requests(
    pool: &PgPool,
    session_id: Uuid,
    limit: Option<i64>,
    offset: Option<i64>,
) -> anyhow::Result<Option<Value>> {
    let page = RequestListPage::from_query(limit, offset);
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

    let rows = sqlx::query(LIST_SESSION_REQUESTS_SQL)
        .bind(session_id)
        .bind(page.fetch_limit())
        .bind(page.offset)
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;

    let has_more = rows.len() > page.limit as usize;
    let items: Vec<Value> = rows
        .into_iter()
        .take(page.limit as usize)
        .map(request_summary_row)
        .collect();

    Ok(Some(json!({
        "session_id": session_id,
        "items": items,
        "page": {
            "limit": page.limit,
            "offset": page.offset,
            "has_more": has_more,
            "next_offset": page.next_offset(has_more),
        },
    })))
}

pub async fn recent_error_requests(
    pool: &PgPool,
    since_hours: Option<i64>,
    limit: Option<i64>,
) -> anyhow::Result<Value> {
    let window = RecentErrorWindow::from_query(since_hours, limit);
    let cutoff = window.cutoff(Utc::now());
    let mut tx = begin_api_read_tx(pool).await?;
    let rows = sqlx::query(RECENT_ERROR_REQUESTS_SQL)
        .bind(cutoff)
        .bind(window.fetch_limit())
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;

    let has_more = rows.len() > window.limit as usize;
    let items: Vec<Value> = rows
        .into_iter()
        .take(window.limit as usize)
        .map(request_summary_row)
        .collect();

    Ok(json!({
        "window": {
            "since_hours": window.since_hours,
            "started_at_gte": cutoff,
            "limit": window.limit,
        },
        "items": items,
        "page": {
            "limit": window.limit,
            "has_more": has_more,
        },
    }))
}

pub async fn request_facets(
    pool: &PgPool,
    since_hours: Option<i64>,
    limit: Option<i64>,
) -> anyhow::Result<Value> {
    let window = RequestFacetWindow::from_query(since_hours, limit);
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
        "window": {
            "since_hours": window.since_hours,
            "started_at_gte": cutoff,
            "limit": window.limit,
        },
        "facets": {
            "models": text_facet_rows(models),
            "upstream_hosts": text_facet_rows(upstream_hosts),
            "request_kinds": text_facet_rows(request_kinds),
            "statuses": int_facet_rows(statuses),
            "status_classes": text_facet_rows(status_classes),
            "error_states": bool_facet_rows(error_states),
        },
    }))
}

pub async fn get_request(
    pool: &PgPool,
    id: Uuid,
    body_decode_limit: usize,
) -> anyhow::Result<Option<Value>> {
    let mut tx = begin_api_read_tx(pool).await?;
    let row = sqlx::query(
        r#"
        SELECT id, started_at, completed_at, method, original_uri, upstream_url, upstream_host,
               status, error, request_kind, model, api_key_hash, session_key, session_id,
               ttft_ms, duration_ms, bytes_in, bytes_out, request_headers, response_headers,
               request_body_compressed, response_body_compressed, request_body_bytes, response_body_bytes,
               request_body_truncated, response_body_truncated, content_type, plugin_metadata, tags
        FROM request_traces
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;

    let Some(row) = row else {
        return Ok(None);
    };
    let request_body: Vec<u8> = row.get("request_body_compressed");
    let response_body: Vec<u8> = row.get("response_body_compressed");
    let request_body =
        String::from_utf8_lossy(&decompress_with_limit(&request_body, body_decode_limit)?)
            .to_string();
    let response_body =
        String::from_utf8_lossy(&decompress_with_limit(&response_body, body_decode_limit)?)
            .to_string();

    Ok(Some(json!({
        "id": row.get::<Uuid, _>("id"),
        "started_at": row.get::<DateTime<Utc>, _>("started_at"),
        "completed_at": row.try_get::<Option<DateTime<Utc>>, _>("completed_at").ok().flatten(),
        "method": row.get::<String, _>("method"),
        "original_uri": row.get::<String, _>("original_uri"),
        "upstream_url": row.get::<String, _>("upstream_url"),
        "upstream_host": row.try_get::<Option<String>, _>("upstream_host").ok().flatten(),
        "status": row.try_get::<Option<i32>, _>("status").ok().flatten(),
        "error": row.try_get::<Option<String>, _>("error").ok().flatten(),
        "request_kind": row.get::<String, _>("request_kind"),
        "model": row.try_get::<Option<String>, _>("model").ok().flatten(),
        "api_key_hash": row.try_get::<Option<String>, _>("api_key_hash").ok().flatten(),
        "session_key": row.try_get::<Option<String>, _>("session_key").ok().flatten(),
        "session_id": row.try_get::<Option<Uuid>, _>("session_id").ok().flatten(),
        "ttft_ms": row.try_get::<Option<i64>, _>("ttft_ms").ok().flatten(),
        "duration_ms": row.try_get::<Option<i64>, _>("duration_ms").ok().flatten(),
        "bytes_in": row.get::<i64, _>("bytes_in"),
        "bytes_out": row.get::<i64, _>("bytes_out"),
        "request_headers": row.get::<Value, _>("request_headers"),
        "response_headers": row.get::<Value, _>("response_headers"),
        "request_body": request_body,
        "response_body": response_body,
        "request_body_bytes": row.get::<i64, _>("request_body_bytes"),
        "response_body_bytes": row.get::<i64, _>("response_body_bytes"),
        "request_body_truncated": row.get::<bool, _>("request_body_truncated"),
        "response_body_truncated": row.get::<bool, _>("response_body_truncated"),
        "content_type": row.try_get::<Option<String>, _>("content_type").ok().flatten(),
        "plugin_metadata": row.get::<Value, _>("plugin_metadata"),
        "tags": row.get::<Vec<String>, _>("tags"),
    })))
}

fn request_summary_row(row: sqlx::postgres::PgRow) -> Value {
    json!({
        "id": row.get::<Uuid, _>("id"),
        "started_at": row.get::<DateTime<Utc>, _>("started_at"),
        "completed_at": row.try_get::<Option<DateTime<Utc>>, _>("completed_at").ok().flatten(),
        "method": row.get::<String, _>("method"),
        "original_uri": row.get::<String, _>("original_uri"),
        "upstream_url": row.get::<String, _>("upstream_url"),
        "upstream_host": row.try_get::<Option<String>, _>("upstream_host").ok().flatten(),
        "status": row.try_get::<Option<i32>, _>("status").ok().flatten(),
        "error": row.try_get::<Option<String>, _>("error").ok().flatten(),
        "request_kind": row.get::<String, _>("request_kind"),
        "model": row.try_get::<Option<String>, _>("model").ok().flatten(),
        "api_key_hash": row.try_get::<Option<String>, _>("api_key_hash").ok().flatten(),
        "session_id": row.try_get::<Option<Uuid>, _>("session_id").ok().flatten(),
        "ttft_ms": row.try_get::<Option<i64>, _>("ttft_ms").ok().flatten(),
        "duration_ms": row.try_get::<Option<i64>, _>("duration_ms").ok().flatten(),
        "bytes_in": row.get::<i64, _>("bytes_in"),
        "bytes_out": row.get::<i64, _>("bytes_out"),
        "request_body_truncated": row.get::<bool, _>("request_body_truncated"),
        "response_body_truncated": row.get::<bool, _>("response_body_truncated"),
        "plugin_metadata": row.get::<Value, _>("plugin_metadata"),
        "tags": row.get::<Vec<String>, _>("tags"),
    })
}

pub async fn list_sessions(
    pool: &PgPool,
    q: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> anyhow::Result<Value> {
    let q = q.map(|value| escape_like(&value));
    let page = SessionListPage::from_query(limit, offset);
    let mut tx = begin_api_read_tx(pool).await?;
    let rows = sqlx::query(LIST_SESSIONS_SQL)
        .bind(q)
        .bind(page.fetch_limit())
        .bind(page.offset)
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;

    let has_more = rows.len() > page.limit as usize;
    let items: Vec<Value> = rows
        .into_iter()
        .take(page.limit as usize)
        .map(|row| {
            json!({
                "id": row.get::<Uuid, _>("id"),
                "session_key": row.get::<String, _>("session_key"),
                "first_seen": row.get::<DateTime<Utc>, _>("first_seen"),
                "last_seen": row.get::<DateTime<Utc>, _>("last_seen"),
                "user_id": row.try_get::<Option<String>, _>("user_id").ok().flatten(),
                "user_name": row.try_get::<Option<String>, _>("user_name").ok().flatten(),
                "request_count": row.get::<i64, _>("request_count"),
                "max_duration_ms": row.get::<i64, _>("max_duration_ms"),
            })
        })
        .collect();
    Ok(json!({
        "items": items,
        "page": {
            "limit": page.limit,
            "offset": page.offset,
            "has_more": has_more,
            "next_offset": page.next_offset(has_more),
        },
    }))
}

pub async fn get_session(
    pool: &PgPool,
    id: Uuid,
    messages_limit: Option<i64>,
    messages_offset: Option<i64>,
) -> anyhow::Result<Option<Value>> {
    let mut tx = begin_api_read_tx(pool).await?;
    let session = sqlx::query(
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

    let page = SessionMessagePage::from_query(messages_limit, messages_offset);
    let request_stats = sqlx::query(SESSION_REQUEST_STATS_SQL)
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    let messages = sqlx::query(
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
    let messages: Vec<Value> = messages
        .into_iter()
        .take(page.limit as usize)
        .map(|row| {
            json!({
                "id": row.get::<i64, _>("id"),
                "request_id": row.get::<Uuid, _>("request_id"),
                "role": row.get::<String, _>("role"),
                "content": row.get::<String, _>("content"),
                "created_at": row.get::<DateTime<Utc>, _>("created_at"),
            })
        })
        .collect();

    Ok(Some(json!({
        "id": session.get::<Uuid, _>("id"),
        "session_key": session.get::<String, _>("session_key"),
        "first_seen": session.get::<DateTime<Utc>, _>("first_seen"),
        "last_seen": session.get::<DateTime<Utc>, _>("last_seen"),
        "user_id": session.try_get::<Option<String>, _>("user_id").ok().flatten(),
        "user_name": session.try_get::<Option<String>, _>("user_name").ok().flatten(),
        "summary": session.get::<Value, _>("summary"),
        "request_stats": {
            "request_count": request_stats.get::<i64, _>("request_count"),
            "error_count": request_stats.get::<i64, _>("error_count"),
            "bytes_in": request_stats.get::<i64, _>("bytes_in"),
            "bytes_out": request_stats.get::<i64, _>("bytes_out"),
            "captured_bytes": request_stats.get::<i64, _>("captured_bytes"),
            "avg_duration_ms": request_stats.try_get::<Option<i64>, _>("avg_duration_ms").ok().flatten(),
            "max_duration_ms": request_stats.try_get::<Option<i64>, _>("max_duration_ms").ok().flatten(),
            "avg_ttft_ms": request_stats.try_get::<Option<i64>, _>("avg_ttft_ms").ok().flatten(),
            "max_ttft_ms": request_stats.try_get::<Option<i64>, _>("max_ttft_ms").ok().flatten(),
            "first_request_at": request_stats.try_get::<Option<DateTime<Utc>>, _>("first_request_at").ok().flatten(),
            "last_request_at": request_stats.try_get::<Option<DateTime<Utc>>, _>("last_request_at").ok().flatten(),
        },
        "messages": messages,
        "messages_page": {
            "limit": page.limit,
            "offset": page.offset,
            "has_more": has_more,
            "next_offset": page.next_offset(has_more),
        },
    })))
}

pub async fn list_audit_events(
    pool: &PgPool,
    event_type: Option<String>,
    user_id: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> anyhow::Result<Value> {
    let page = AuditEventPage::from_query(limit, offset);
    let mut tx = begin_api_read_tx(pool).await?;
    let rows = sqlx::query(
        r#"
        SELECT id, created_at, event_type, user_id, remote_addr, detail
        FROM ui_audit_events
        WHERE ($1::text IS NULL OR event_type = $1)
          AND ($2::text IS NULL OR user_id = $2)
        ORDER BY created_at DESC, id DESC
        LIMIT $3 OFFSET $4
        "#,
    )
    .bind(event_type)
    .bind(user_id)
    .bind(page.fetch_limit())
    .bind(page.offset)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let has_more = rows.len() > page.limit as usize;
    let items: Vec<Value> = rows
        .into_iter()
        .take(page.limit as usize)
        .map(|row| {
            json!({
                "id": row.get::<i64, _>("id"),
                "created_at": row.get::<DateTime<Utc>, _>("created_at"),
                "event_type": row.get::<String, _>("event_type"),
                "user_id": row.try_get::<Option<String>, _>("user_id").ok().flatten(),
                "remote_addr": row.try_get::<Option<String>, _>("remote_addr").ok().flatten(),
                "detail": row.get::<Value, _>("detail"),
            })
        })
        .collect();

    Ok(json!({
        "items": items,
        "page": {
            "limit": page.limit,
            "offset": page.offset,
            "has_more": has_more,
            "next_offset": page.next_offset(has_more),
        },
    }))
}

pub async fn usage_summary(
    pool: &PgPool,
    since_hours: Option<i64>,
    limit: Option<i64>,
) -> anyhow::Result<Value> {
    let window = UsageSummaryWindow::from_query(since_hours, limit);
    let cutoff = window.cutoff(Utc::now());
    let mut tx = begin_api_read_tx(pool).await?;

    let totals = sqlx::query(
        r#"
        SELECT COUNT(*)::bigint AS request_count,
               COUNT(*) FILTER (WHERE error IS NOT NULL OR status >= 500)::bigint AS error_count,
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

    let top_models = sqlx::query(
        r#"
        SELECT COALESCE(NULLIF(model, ''), 'unknown') AS name,
               COUNT(*)::bigint AS request_count,
               COUNT(*) FILTER (WHERE error IS NOT NULL OR status >= 500)::bigint AS error_count,
               AVG(duration_ms)::bigint AS avg_duration_ms,
               AVG(ttft_ms)::bigint AS avg_ttft_ms
        FROM trace_requests
        WHERE started_at >= $1
        GROUP BY 1
        ORDER BY request_count DESC, name ASC
        LIMIT $2
        "#,
    )
    .bind(cutoff)
    .bind(window.limit)
    .fetch_all(&mut *tx)
    .await?;

    let top_upstreams = sqlx::query(
        r#"
        SELECT COALESCE(NULLIF(upstream_host, ''), 'unknown') AS name,
               COUNT(*)::bigint AS request_count,
               COUNT(*) FILTER (WHERE error IS NOT NULL OR status >= 500)::bigint AS error_count,
               AVG(duration_ms)::bigint AS avg_duration_ms,
               AVG(ttft_ms)::bigint AS avg_ttft_ms
        FROM trace_requests
        WHERE started_at >= $1
        GROUP BY 1
        ORDER BY request_count DESC, name ASC
        LIMIT $2
        "#,
    )
    .bind(cutoff)
    .bind(window.limit)
    .fetch_all(&mut *tx)
    .await?;

    let status_classes = sqlx::query(
        r#"
        SELECT CASE
                   WHEN status IS NULL THEN 'no_status'
                   WHEN status BETWEEN 100 AND 199 THEN '1xx'
                   WHEN status BETWEEN 200 AND 299 THEN '2xx'
                   WHEN status BETWEEN 300 AND 399 THEN '3xx'
                   WHEN status BETWEEN 400 AND 499 THEN '4xx'
                   WHEN status BETWEEN 500 AND 599 THEN '5xx'
                   ELSE 'other'
               END AS name,
               COUNT(*)::bigint AS request_count
        FROM trace_requests
        WHERE started_at >= $1
        GROUP BY 1
        ORDER BY request_count DESC, name ASC
        "#,
    )
    .bind(cutoff)
    .fetch_all(&mut *tx)
    .await?;

    let request_kinds = sqlx::query(
        r#"
        SELECT request_kind AS name,
               COUNT(*)::bigint AS request_count,
               COUNT(*) FILTER (WHERE error IS NOT NULL OR status >= 500)::bigint AS error_count,
               AVG(duration_ms)::bigint AS avg_duration_ms,
               AVG(ttft_ms)::bigint AS avg_ttft_ms
        FROM trace_requests
        WHERE started_at >= $1
        GROUP BY request_kind
        ORDER BY request_count DESC, name ASC
        LIMIT $2
        "#,
    )
    .bind(cutoff)
    .bind(window.limit)
    .fetch_all(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(json!({
        "window": {
            "since_hours": window.since_hours,
            "started_at_gte": cutoff,
            "limit": window.limit,
        },
        "totals": {
            "request_count": totals.get::<i64, _>("request_count"),
            "error_count": totals.get::<i64, _>("error_count"),
            "bytes_in": totals.get::<i64, _>("bytes_in"),
            "bytes_out": totals.get::<i64, _>("bytes_out"),
            "captured_bytes": totals.get::<i64, _>("captured_bytes"),
            "avg_duration_ms": totals.try_get::<Option<i64>, _>("avg_duration_ms").ok().flatten(),
            "avg_ttft_ms": totals.try_get::<Option<i64>, _>("avg_ttft_ms").ok().flatten(),
        },
        "top_models": named_metric_rows(top_models),
        "top_upstreams": named_metric_rows(top_upstreams),
        "status_classes": count_rows(status_classes),
        "request_kinds": named_metric_rows(request_kinds),
    }))
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

    let points: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({
                "bucket": row.get::<DateTime<Utc>, _>("bucket"),
                "request_count": row.get::<i64, _>("request_count"),
                "error_count": row.get::<i64, _>("error_count"),
                "captured_bytes": row.get::<i64, _>("captured_bytes"),
                "avg_duration_ms": row.try_get::<Option<i64>, _>("avg_duration_ms").ok().flatten(),
                "avg_ttft_ms": row.try_get::<Option<i64>, _>("avg_ttft_ms").ok().flatten(),
            })
        })
        .collect();

    Ok(json!({
        "window": {
            "since_hours": window.since_hours,
            "started_at_gte": cutoff,
            "bucket": window.bucket.as_str(),
        },
        "points": points,
    }))
}

fn named_metric_rows(rows: Vec<sqlx::postgres::PgRow>) -> Vec<Value> {
    rows.into_iter()
        .map(|row| {
            json!({
                "name": row.get::<String, _>("name"),
                "request_count": row.get::<i64, _>("request_count"),
                "error_count": row.get::<i64, _>("error_count"),
                "avg_duration_ms": row.try_get::<Option<i64>, _>("avg_duration_ms").ok().flatten(),
                "avg_ttft_ms": row.try_get::<Option<i64>, _>("avg_ttft_ms").ok().flatten(),
            })
        })
        .collect()
}

fn text_facet_rows(rows: Vec<sqlx::postgres::PgRow>) -> Vec<Value> {
    rows.into_iter()
        .map(|row| {
            json!({
                "value": row.get::<String, _>("value"),
                "request_count": row.get::<i64, _>("request_count"),
            })
        })
        .collect()
}

fn int_facet_rows(rows: Vec<sqlx::postgres::PgRow>) -> Vec<Value> {
    rows.into_iter()
        .map(|row| {
            json!({
                "value": row.get::<i32, _>("value"),
                "request_count": row.get::<i64, _>("request_count"),
            })
        })
        .collect()
}

fn bool_facet_rows(rows: Vec<sqlx::postgres::PgRow>) -> Vec<Value> {
    rows.into_iter()
        .map(|row| {
            json!({
                "value": row.get::<bool, _>("value"),
                "request_count": row.get::<i64, _>("request_count"),
            })
        })
        .collect()
}

fn count_rows(rows: Vec<sqlx::postgres::PgRow>) -> Vec<Value> {
    rows.into_iter()
        .map(|row| {
            json!({
                "name": row.get::<String, _>("name"),
                "request_count": row.get::<i64, _>("request_count"),
            })
        })
        .collect()
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

pub fn structured_query_schema() -> Value {
    json!({
        "limits": {
            "max_fields": MAX_STRUCTURED_QUERY_FIELDS,
            "max_filters": MAX_STRUCTURED_QUERY_FILTERS,
            "max_order_by": MAX_STRUCTURED_QUERY_ORDER_BY,
            "max_limit": 500,
            "max_string_value_bytes": MAX_STRUCTURED_QUERY_STRING_VALUE_BYTES,
            "max_json_value_bytes": MAX_STRUCTURED_QUERY_JSON_VALUE_BYTES,
        },
        "sort_directions": ["asc", "desc"],
        "plugin_metadata": {
            "dataset": "requests",
            "field_prefix": PLUGIN_METADATA_FIELD_PREFIX,
            "filter_kind": FilterKind::JsonPath.as_str(),
            "operators": FilterKind::JsonPath.operators(),
            "max_segments": MAX_PLUGIN_METADATA_PATH_SEGMENTS,
            "max_segment_bytes": MAX_PLUGIN_METADATA_PATH_SEGMENT_LEN,
            "segment_pattern": "[A-Za-z0-9_-]+",
        },
        "datasets": DATASETS
            .iter()
            .map(dataset_schema)
            .collect::<Vec<_>>(),
    })
}

fn dataset_schema(dataset: &DatasetSpec) -> Value {
    json!({
        "name": dataset.name,
        "default_fields": dataset.default_fields,
        "default_order": dataset
            .default_order
            .iter()
            .map(default_order_schema)
            .collect::<Vec<_>>(),
        "fields": dataset
            .fields
            .iter()
            .map(field_schema)
            .collect::<Vec<_>>(),
    })
}

fn field_schema(field: &FieldSpec) -> Value {
    json!({
        "name": field.name,
        "filter_kind": field.filter.map(|kind| kind.as_str()),
        "operators": field
            .filter
            .map(|kind| kind.operators())
            .unwrap_or_default(),
    })
}

fn default_order_schema(order: &DefaultOrder) -> Value {
    json!({
        "field": order.field,
        "direction": order.direction.as_str(),
    })
}

static REQUEST_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "id",
        sql: "id",
        filter: Some(FilterKind::Uuid),
    },
    FieldSpec {
        name: "started_at",
        sql: "started_at",
        filter: Some(FilterKind::Timestamp),
    },
    FieldSpec {
        name: "completed_at",
        sql: "completed_at",
        filter: Some(FilterKind::Timestamp),
    },
    FieldSpec {
        name: "method",
        sql: "method",
        filter: Some(FilterKind::Text),
    },
    FieldSpec {
        name: "original_uri",
        sql: "original_uri",
        filter: Some(FilterKind::Text),
    },
    FieldSpec {
        name: "upstream_url",
        sql: "upstream_url",
        filter: Some(FilterKind::Text),
    },
    FieldSpec {
        name: "upstream_host",
        sql: "upstream_host",
        filter: Some(FilterKind::Text),
    },
    FieldSpec {
        name: "status",
        sql: "status",
        filter: Some(FilterKind::Int),
    },
    FieldSpec {
        name: "error",
        sql: "error",
        filter: Some(FilterKind::Text),
    },
    FieldSpec {
        name: "request_kind",
        sql: "request_kind",
        filter: Some(FilterKind::Text),
    },
    FieldSpec {
        name: "model",
        sql: "model",
        filter: Some(FilterKind::Text),
    },
    FieldSpec {
        name: "api_key_hash",
        sql: "api_key_hash",
        filter: Some(FilterKind::Text),
    },
    FieldSpec {
        name: "session_key",
        sql: "session_key",
        filter: Some(FilterKind::Text),
    },
    FieldSpec {
        name: "session_id",
        sql: "session_id",
        filter: Some(FilterKind::Uuid),
    },
    FieldSpec {
        name: "ttft_ms",
        sql: "ttft_ms",
        filter: Some(FilterKind::Int),
    },
    FieldSpec {
        name: "duration_ms",
        sql: "duration_ms",
        filter: Some(FilterKind::Int),
    },
    FieldSpec {
        name: "bytes_in",
        sql: "bytes_in",
        filter: Some(FilterKind::Int),
    },
    FieldSpec {
        name: "bytes_out",
        sql: "bytes_out",
        filter: Some(FilterKind::Int),
    },
    FieldSpec {
        name: "request_headers",
        sql: "request_headers",
        filter: Some(FilterKind::Json),
    },
    FieldSpec {
        name: "response_headers",
        sql: "response_headers",
        filter: Some(FilterKind::Json),
    },
    FieldSpec {
        name: "request_body_bytes",
        sql: "request_body_bytes",
        filter: Some(FilterKind::Int),
    },
    FieldSpec {
        name: "response_body_bytes",
        sql: "response_body_bytes",
        filter: Some(FilterKind::Int),
    },
    FieldSpec {
        name: "request_body_truncated",
        sql: "request_body_truncated",
        filter: Some(FilterKind::Bool),
    },
    FieldSpec {
        name: "response_body_truncated",
        sql: "response_body_truncated",
        filter: Some(FilterKind::Bool),
    },
    FieldSpec {
        name: "content_type",
        sql: "content_type",
        filter: Some(FilterKind::Text),
    },
    FieldSpec {
        name: "plugin_metadata",
        sql: "plugin_metadata",
        filter: Some(FilterKind::Json),
    },
    FieldSpec {
        name: "tags",
        sql: "tags",
        filter: Some(FilterKind::TextArray),
    },
];

static SESSION_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "id",
        sql: "id",
        filter: Some(FilterKind::Uuid),
    },
    FieldSpec {
        name: "session_key",
        sql: "session_key",
        filter: Some(FilterKind::Text),
    },
    FieldSpec {
        name: "first_seen",
        sql: "first_seen",
        filter: Some(FilterKind::Timestamp),
    },
    FieldSpec {
        name: "last_seen",
        sql: "last_seen",
        filter: Some(FilterKind::Timestamp),
    },
    FieldSpec {
        name: "user_id",
        sql: "user_id",
        filter: Some(FilterKind::Text),
    },
    FieldSpec {
        name: "user_name",
        sql: "user_name",
        filter: Some(FilterKind::Text),
    },
    FieldSpec {
        name: "summary",
        sql: "summary",
        filter: Some(FilterKind::Json),
    },
];

static MESSAGE_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "id",
        sql: "id",
        filter: Some(FilterKind::Int),
    },
    FieldSpec {
        name: "request_id",
        sql: "request_id",
        filter: Some(FilterKind::Uuid),
    },
    FieldSpec {
        name: "session_id",
        sql: "session_id",
        filter: Some(FilterKind::Uuid),
    },
    FieldSpec {
        name: "role",
        sql: "role",
        filter: Some(FilterKind::Text),
    },
    FieldSpec {
        name: "content",
        sql: "content",
        filter: Some(FilterKind::Text),
    },
    FieldSpec {
        name: "created_at",
        sql: "created_at",
        filter: Some(FilterKind::Timestamp),
    },
];

static ROLLUP_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "bucket",
        sql: "bucket",
        filter: Some(FilterKind::Timestamp),
    },
    FieldSpec {
        name: "last_seen",
        sql: "last_seen",
        filter: Some(FilterKind::Timestamp),
    },
    FieldSpec {
        name: "total",
        sql: "total",
        filter: Some(FilterKind::Int),
    },
    FieldSpec {
        name: "errors",
        sql: "errors",
        filter: Some(FilterKind::Int),
    },
    FieldSpec {
        name: "captured_bytes",
        sql: "captured_bytes",
        filter: Some(FilterKind::Int),
    },
    FieldSpec {
        name: "duration_count",
        sql: "duration_count",
        filter: Some(FilterKind::Int),
    },
    FieldSpec {
        name: "duration_sum_ms",
        sql: "duration_sum_ms",
        filter: Some(FilterKind::Int),
    },
    FieldSpec {
        name: "ttft_count",
        sql: "ttft_count",
        filter: Some(FilterKind::Int),
    },
    FieldSpec {
        name: "ttft_sum_ms",
        sql: "ttft_sum_ms",
        filter: Some(FilterKind::Int),
    },
];

static DATASETS: &[DatasetSpec] = &[
    DatasetSpec {
        name: "requests",
        relation: "trace_requests",
        fields: REQUEST_FIELDS,
        default_fields: &[
            "id",
            "started_at",
            "method",
            "original_uri",
            "upstream_host",
            "status",
            "request_kind",
            "model",
            "duration_ms",
            "bytes_in",
            "bytes_out",
            "tags",
        ],
        default_order: &[DefaultOrder {
            field: "started_at",
            direction: SortDirection::Desc,
        }],
    },
    DatasetSpec {
        name: "sessions",
        relation: "trace_sessions",
        fields: SESSION_FIELDS,
        default_fields: &[
            "id",
            "session_key",
            "first_seen",
            "last_seen",
            "user_id",
            "user_name",
        ],
        default_order: &[DefaultOrder {
            field: "last_seen",
            direction: SortDirection::Desc,
        }],
    },
    DatasetSpec {
        name: "messages",
        relation: "trace_messages",
        fields: MESSAGE_FIELDS,
        default_fields: &[
            "id",
            "request_id",
            "session_id",
            "role",
            "content",
            "created_at",
        ],
        default_order: &[DefaultOrder {
            field: "created_at",
            direction: SortDirection::Desc,
        }],
    },
    DatasetSpec {
        name: "rollups_minute",
        relation: "trace_rollups_minute",
        fields: ROLLUP_FIELDS,
        default_fields: &[
            "bucket",
            "last_seen",
            "total",
            "errors",
            "captured_bytes",
            "duration_count",
            "duration_sum_ms",
            "ttft_count",
            "ttft_sum_ms",
        ],
        default_order: &[DefaultOrder {
            field: "bucket",
            direction: SortDirection::Desc,
        }],
    },
];

pub async fn run_structured_query(
    pool: &PgPool,
    request: StructuredQuery,
) -> Result<Value, StructuredQueryError> {
    let dataset = dataset_spec(&request.dataset).map_err(StructuredQueryError::invalid)?;
    let selected = selected_fields(dataset, request.fields.as_deref())
        .map_err(StructuredQueryError::invalid)?;
    let order_by =
        selected_order(dataset, &request.order_by).map_err(StructuredQueryError::invalid)?;
    validate_structured_query_filters(&request.filters).map_err(StructuredQueryError::invalid)?;
    let limit = request.limit.unwrap_or(100).clamp(1, 500);

    let mut builder = QueryBuilder::<Postgres>::new(
        "SELECT COALESCE(jsonb_agg(to_jsonb(q)), '[]'::jsonb) AS rows FROM (SELECT ",
    );

    for (index, field) in selected.iter().enumerate() {
        if index > 0 {
            builder.push(", ");
        }
        field.append_sql(&mut builder);
        builder.push(" AS ");
        append_identifier(&mut builder, field.name());
    }

    builder.push(" FROM ").push(dataset.relation);

    if !request.filters.is_empty() {
        builder.push(" WHERE ");
        for (index, filter) in request.filters.iter().enumerate() {
            if index > 0 {
                builder.push(" AND ");
            }
            append_filter(&mut builder, dataset, filter).map_err(StructuredQueryError::invalid)?;
        }
    }

    if !order_by.is_empty() {
        builder.push(" ORDER BY ");
        for (index, (field, direction)) in order_by.iter().enumerate() {
            if index > 0 {
                builder.push(", ");
            }
            field.append_sql(&mut builder);
            builder.push(match direction {
                SortDirection::Asc => " ASC",
                SortDirection::Desc => " DESC",
            });
        }
    }

    builder.push(" LIMIT ");
    builder.push_bind(limit);
    builder.push(") q");

    let mut tx = begin_api_read_tx(pool)
        .await
        .map_err(StructuredQueryError::execution)?;
    let rows: Value = builder
        .build_query_scalar()
        .fetch_one(&mut *tx)
        .await
        .map_err(StructuredQueryError::execution)?;
    tx.commit().await.map_err(StructuredQueryError::execution)?;

    Ok(json!({
        "dataset": dataset.name,
        "fields": selected.iter().map(|field| field.name()).collect::<Vec<_>>(),
        "rows": rows,
        "limit": limit,
    }))
}

fn dataset_spec(name: &str) -> anyhow::Result<&'static DatasetSpec> {
    DATASETS
        .iter()
        .find(|dataset| dataset.name == name)
        .ok_or_else(|| anyhow::anyhow!("unknown query dataset {name}"))
}

fn selected_fields(
    dataset: &'static DatasetSpec,
    requested: Option<&[String]>,
) -> anyhow::Result<Vec<QueryField>> {
    let names: Vec<&str> = match requested {
        Some([]) => anyhow::bail!("fields must not be empty"),
        Some(fields) => {
            if fields.len() > MAX_STRUCTURED_QUERY_FIELDS {
                anyhow::bail!(
                    "fields must contain at most {} entries",
                    MAX_STRUCTURED_QUERY_FIELDS
                );
            }
            fields.iter().map(String::as_str).collect()
        }
        None => dataset.default_fields.to_vec(),
    };

    let mut selected = Vec::with_capacity(names.len());
    for name in names {
        let field = query_field(dataset, name)?;
        if selected
            .iter()
            .any(|selected_field: &QueryField| selected_field.name() == field.name())
        {
            continue;
        }
        selected.push(field);
    }

    if selected.is_empty() {
        anyhow::bail!("at least one field must be selected");
    }
    Ok(selected)
}

fn selected_order(
    dataset: &'static DatasetSpec,
    requested: &[QueryOrder],
) -> anyhow::Result<Vec<(QueryField, SortDirection)>> {
    if requested.len() > MAX_STRUCTURED_QUERY_ORDER_BY {
        anyhow::bail!(
            "order_by must contain at most {} entries",
            MAX_STRUCTURED_QUERY_ORDER_BY
        );
    }

    if requested.is_empty() {
        return dataset
            .default_order
            .iter()
            .map(|order| Ok((query_field(dataset, order.field)?, order.direction)))
            .collect();
    }

    requested
        .iter()
        .map(|order| Ok((query_field(dataset, &order.field)?, order.direction)))
        .collect()
}

fn field_spec(dataset: &'static DatasetSpec, name: &str) -> anyhow::Result<&'static FieldSpec> {
    dataset
        .fields
        .iter()
        .find(|field| field.name == name)
        .ok_or_else(|| anyhow::anyhow!("field {name} is not allowed for dataset {}", dataset.name))
}

fn query_field(dataset: &'static DatasetSpec, name: &str) -> anyhow::Result<QueryField> {
    if let Ok(field) = field_spec(dataset, name) {
        return Ok(QueryField::Static(field));
    }

    if dataset.name == "requests"
        && let Some(path) = plugin_metadata_path(name)?
    {
        return Ok(QueryField::PluginMetadataPath {
            name: name.to_string(),
            path,
        });
    }

    anyhow::bail!("field {name} is not allowed for dataset {}", dataset.name)
}

fn validate_structured_query_filters(filters: &[QueryFilter]) -> anyhow::Result<()> {
    if filters.len() > MAX_STRUCTURED_QUERY_FILTERS {
        anyhow::bail!(
            "filters must contain at most {} entries",
            MAX_STRUCTURED_QUERY_FILTERS
        );
    }
    Ok(())
}

const PLUGIN_METADATA_FIELD_PREFIX: &str = "plugin_metadata.";
const MAX_PLUGIN_METADATA_PATH_SEGMENTS: usize = 16;
const MAX_PLUGIN_METADATA_PATH_SEGMENT_LEN: usize = 64;

fn plugin_metadata_path(name: &str) -> anyhow::Result<Option<Vec<String>>> {
    let Some(path) = name.strip_prefix(PLUGIN_METADATA_FIELD_PREFIX) else {
        return Ok(None);
    };
    if path.is_empty() {
        anyhow::bail!("plugin_metadata path must not be empty");
    }

    let segments: Vec<String> = path.split('.').map(str::to_string).collect();
    if segments.len() > MAX_PLUGIN_METADATA_PATH_SEGMENTS {
        anyhow::bail!(
            "plugin_metadata path must have at most {} segments",
            MAX_PLUGIN_METADATA_PATH_SEGMENTS
        );
    }

    for segment in &segments {
        if !valid_plugin_metadata_path_segment(segment) {
            anyhow::bail!(
                "plugin_metadata path segment {segment:?} must contain only ASCII letters, digits, '_' or '-'"
            );
        }
    }

    Ok(Some(segments))
}

fn valid_plugin_metadata_path_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= MAX_PLUGIN_METADATA_PATH_SEGMENT_LEN
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn append_identifier(builder: &mut QueryBuilder<'_, Postgres>, name: &str) {
    debug_assert!(!name.contains('"'));
    builder.push("\"").push(name).push("\"");
}

fn append_filter(
    builder: &mut QueryBuilder<'_, Postgres>,
    dataset: &'static DatasetSpec,
    filter: &QueryFilter,
) -> anyhow::Result<()> {
    let field = query_field(dataset, &filter.field)?;
    let kind = field
        .filter()
        .ok_or_else(|| anyhow::anyhow!("field {} cannot be filtered", filter.field))?;

    match filter.op {
        QueryOp::IsNull => {
            append_null_filter(builder, &field, true);
            return Ok(());
        }
        QueryOp::IsNotNull => {
            append_null_filter(builder, &field, false);
            return Ok(());
        }
        QueryOp::Eq | QueryOp::Ne if filter.value.as_ref().is_some_and(Value::is_null) => {
            append_null_filter(builder, &field, matches!(filter.op, QueryOp::Eq));
            return Ok(());
        }
        _ => {}
    }

    let value = filter
        .value
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("filter {} requires a value", filter.field))?;

    match kind {
        FilterKind::Text => append_text_filter(builder, &field, filter.op, value),
        FilterKind::Int => append_int_filter(builder, &field, filter.op, value),
        FilterKind::Bool => append_bool_filter(builder, &field, filter.op, value),
        FilterKind::Timestamp => append_timestamp_filter(builder, &field, filter.op, value),
        FilterKind::Uuid => append_uuid_filter(builder, &field, filter.op, value),
        FilterKind::Json | FilterKind::JsonPath => {
            append_json_filter(builder, &field, filter.op, value)
        }
        FilterKind::TextArray => append_text_array_filter(builder, &field, filter.op, value),
    }
}

fn append_null_filter(builder: &mut QueryBuilder<'_, Postgres>, field: &QueryField, is_null: bool) {
    match field {
        QueryField::PluginMetadataPath { .. } => {
            builder.push("(");
            field.append_sql(builder);
            builder.push(if is_null {
                " IS NULL OR "
            } else {
                " IS NOT NULL AND "
            });
            field.append_sql(builder);
            builder.push(if is_null {
                " = 'null'::jsonb"
            } else {
                " <> 'null'::jsonb"
            });
            builder.push(")");
        }
        QueryField::Static(_) => {
            field.append_sql(builder);
            builder.push(if is_null { " IS NULL" } else { " IS NOT NULL" });
        }
    }
}

fn append_text_filter(
    builder: &mut QueryBuilder<'_, Postgres>,
    field: &QueryField,
    op: QueryOp,
    value: &Value,
) -> anyhow::Result<()> {
    let value = value_as_string(field.name(), value)?;
    match op {
        QueryOp::Eq | QueryOp::Ne | QueryOp::Gt | QueryOp::Gte | QueryOp::Lt | QueryOp::Lte => {
            field.append_sql(builder);
            builder.push(comparison_operator(op)?);
            builder.push_bind(value);
        }
        QueryOp::Contains => {
            field.append_sql(builder);
            builder.push(" ILIKE ");
            builder.push_bind(format!("%{}%", escape_like(&value)));
            builder.push(" ESCAPE '\\'");
        }
        QueryOp::IsNull | QueryOp::IsNotNull => unreachable!(),
    }
    Ok(())
}

fn append_int_filter(
    builder: &mut QueryBuilder<'_, Postgres>,
    field: &QueryField,
    op: QueryOp,
    value: &Value,
) -> anyhow::Result<()> {
    let value = value_as_i64(field.name(), value)?;
    field.append_sql(builder);
    builder.push(comparison_operator(op)?);
    builder.push_bind(value);
    Ok(())
}

fn append_bool_filter(
    builder: &mut QueryBuilder<'_, Postgres>,
    field: &QueryField,
    op: QueryOp,
    value: &Value,
) -> anyhow::Result<()> {
    let value = value_as_bool(field.name(), value)?;
    match op {
        QueryOp::Eq | QueryOp::Ne => {
            field.append_sql(builder);
            builder.push(comparison_operator(op)?);
            builder.push_bind(value);
            Ok(())
        }
        _ => anyhow::bail!(
            "operator {:?} is not supported for boolean field {}",
            op,
            field.name()
        ),
    }
}

fn append_timestamp_filter(
    builder: &mut QueryBuilder<'_, Postgres>,
    field: &QueryField,
    op: QueryOp,
    value: &Value,
) -> anyhow::Result<()> {
    let value = value_as_timestamp(field.name(), value)?;
    field.append_sql(builder);
    builder.push(comparison_operator(op)?);
    builder.push_bind(value);
    Ok(())
}

fn append_uuid_filter(
    builder: &mut QueryBuilder<'_, Postgres>,
    field: &QueryField,
    op: QueryOp,
    value: &Value,
) -> anyhow::Result<()> {
    let value = value_as_uuid(field.name(), value)?;
    match op {
        QueryOp::Eq | QueryOp::Ne => {
            field.append_sql(builder);
            builder.push(comparison_operator(op)?);
            builder.push_bind(value);
            Ok(())
        }
        _ => anyhow::bail!(
            "operator {:?} is not supported for uuid field {}",
            op,
            field.name()
        ),
    }
}

fn append_json_filter(
    builder: &mut QueryBuilder<'_, Postgres>,
    field: &QueryField,
    op: QueryOp,
    value: &Value,
) -> anyhow::Result<()> {
    validate_json_filter_value_size(field.name(), value)?;
    match op {
        QueryOp::Eq | QueryOp::Ne => {
            field.append_sql(builder);
            builder.push(comparison_operator(op)?);
            builder.push_bind(value.clone());
            Ok(())
        }
        QueryOp::Contains => {
            field.append_sql(builder);
            builder.push(" @> ");
            builder.push_bind(value.clone());
            Ok(())
        }
        _ => anyhow::bail!(
            "operator {:?} is not supported for json field {}",
            op,
            field.name()
        ),
    }
}

fn append_text_array_filter(
    builder: &mut QueryBuilder<'_, Postgres>,
    field: &QueryField,
    op: QueryOp,
    value: &Value,
) -> anyhow::Result<()> {
    let value = value_as_string(field.name(), value)?;
    match op {
        QueryOp::Contains => {
            builder.push_bind(value);
            builder.push(" = ANY(");
            field.append_sql(builder);
            builder.push(")");
            Ok(())
        }
        _ => anyhow::bail!(
            "operator {:?} is not supported for text array field {}",
            op,
            field.name()
        ),
    }
}

fn comparison_operator(op: QueryOp) -> anyhow::Result<&'static str> {
    match op {
        QueryOp::Eq => Ok(" = "),
        QueryOp::Ne => Ok(" <> "),
        QueryOp::Gt => Ok(" > "),
        QueryOp::Gte => Ok(" >= "),
        QueryOp::Lt => Ok(" < "),
        QueryOp::Lte => Ok(" <= "),
        QueryOp::Contains | QueryOp::IsNull | QueryOp::IsNotNull => {
            anyhow::bail!("operator {:?} is not a scalar comparison", op)
        }
    }
}

fn value_as_string(field: &str, value: &Value) -> anyhow::Result<String> {
    let value = value
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("field {field} requires a string value"))?;
    if value.len() > MAX_STRUCTURED_QUERY_STRING_VALUE_BYTES {
        anyhow::bail!(
            "field {field} string value must be at most {} bytes",
            MAX_STRUCTURED_QUERY_STRING_VALUE_BYTES
        );
    }
    Ok(value)
}

fn validate_json_filter_value_size(field: &str, value: &Value) -> anyhow::Result<()> {
    let value_len = serde_json::to_vec(value)?.len();
    if value_len > MAX_STRUCTURED_QUERY_JSON_VALUE_BYTES {
        anyhow::bail!(
            "field {field} JSON value must be at most {} bytes",
            MAX_STRUCTURED_QUERY_JSON_VALUE_BYTES
        );
    }
    Ok(())
}

fn value_as_i64(field: &str, value: &Value) -> anyhow::Result<i64> {
    value
        .as_i64()
        .ok_or_else(|| anyhow::anyhow!("field {field} requires an integer value"))
}

fn value_as_bool(field: &str, value: &Value) -> anyhow::Result<bool> {
    value
        .as_bool()
        .ok_or_else(|| anyhow::anyhow!("field {field} requires a boolean value"))
}

fn value_as_timestamp(field: &str, value: &Value) -> anyhow::Result<DateTime<Utc>> {
    let value = value_as_string(field, value)?;
    Ok(DateTime::parse_from_rfc3339(&value)
        .map_err(|_| anyhow::anyhow!("field {field} requires an RFC3339 timestamp"))?
        .with_timezone(&Utc))
}

fn value_as_uuid(field: &str, value: &Value) -> anyhow::Result<Uuid> {
    let value = value_as_string(field, value)?;
    Uuid::parse_str(&value).map_err(|_| anyhow::anyhow!("field {field} requires a UUID value"))
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn structured_query_rejects_unknown_dataset() {
        let error = dataset_spec("ui_sessions").unwrap_err().to_string();

        assert!(error.contains("unknown query dataset"));
    }

    #[test]
    fn structured_query_schema_describes_datasets_fields_and_limits() {
        let schema = structured_query_schema();

        assert_eq!(schema["limits"]["max_fields"], MAX_STRUCTURED_QUERY_FIELDS);
        assert_eq!(
            schema["limits"]["max_filters"],
            MAX_STRUCTURED_QUERY_FILTERS
        );
        assert_eq!(
            schema["limits"]["max_order_by"],
            MAX_STRUCTURED_QUERY_ORDER_BY
        );
        assert_eq!(
            schema["limits"]["max_string_value_bytes"],
            MAX_STRUCTURED_QUERY_STRING_VALUE_BYTES
        );
        assert_eq!(
            schema["limits"]["max_json_value_bytes"],
            MAX_STRUCTURED_QUERY_JSON_VALUE_BYTES
        );
        assert_eq!(schema["sort_directions"], json!(["asc", "desc"]));
        assert_eq!(
            schema["plugin_metadata"],
            json!({
                "dataset": "requests",
                "field_prefix": PLUGIN_METADATA_FIELD_PREFIX,
                "filter_kind": "json_path",
                "operators": ["eq", "ne", "contains", "is_null", "is_not_null"],
                "max_segments": MAX_PLUGIN_METADATA_PATH_SEGMENTS,
                "max_segment_bytes": MAX_PLUGIN_METADATA_PATH_SEGMENT_LEN,
                "segment_pattern": "[A-Za-z0-9_-]+",
            })
        );

        let datasets = schema["datasets"].as_array().unwrap();
        let requests = datasets
            .iter()
            .find(|dataset| dataset["name"] == "requests")
            .unwrap();
        assert!(
            requests["default_fields"]
                .as_array()
                .unwrap()
                .contains(&json!("started_at"))
        );
        assert_eq!(
            requests["default_order"],
            json!([{"field": "started_at", "direction": "desc"}])
        );

        let fields = requests["fields"].as_array().unwrap();
        let status = fields
            .iter()
            .find(|field| field["name"] == "status")
            .unwrap();
        assert_eq!(status["filter_kind"], "int");
        assert_eq!(
            status["operators"],
            json!([
                "eq",
                "ne",
                "gt",
                "gte",
                "lt",
                "lte",
                "is_null",
                "is_not_null"
            ])
        );

        let tags = fields.iter().find(|field| field["name"] == "tags").unwrap();
        assert_eq!(tags["filter_kind"], "text_array");
        assert_eq!(
            tags["operators"],
            json!(["contains", "is_null", "is_not_null"])
        );
    }

    #[test]
    fn decompress_with_limit_allows_body_within_limit() {
        let compressed = compress(b"hello").unwrap();
        let decompressed = decompress_with_limit(&compressed, 5).unwrap();

        assert_eq!(decompressed, b"hello");
    }

    #[test]
    fn decompress_with_limit_allows_empty_body_with_zero_limit() {
        let decompressed = decompress_with_limit(&[], 0).unwrap();

        assert!(decompressed.is_empty());
    }

    #[test]
    fn decompress_with_limit_rejects_body_over_limit() {
        let compressed = compress(b"hello").unwrap();
        let error = decompress_with_limit(&compressed, 4)
            .unwrap_err()
            .to_string();

        assert!(error.contains("decompression limit"));
    }

    #[test]
    fn request_list_page_uses_safe_defaults() {
        let page = RequestListPage::from_query(None, None);

        assert_eq!(
            page,
            RequestListPage {
                limit: DEFAULT_REQUEST_LIST_LIMIT,
                offset: 0,
            }
        );
        assert_eq!(page.fetch_limit(), DEFAULT_REQUEST_LIST_LIMIT + 1);
    }

    #[test]
    fn request_list_page_clamps_limit_and_offset() {
        let page = RequestListPage::from_query(Some(i64::MAX), Some(i64::MAX));

        assert_eq!(page.limit, MAX_REQUEST_LIST_LIMIT);
        assert_eq!(page.offset, MAX_REQUEST_LIST_OFFSET);
    }

    #[test]
    fn request_list_page_clamps_negative_values() {
        let page = RequestListPage::from_query(Some(-10), Some(-10));

        assert_eq!(page.limit, 1);
        assert_eq!(page.offset, 0);
    }

    #[test]
    fn request_list_page_reports_next_offset_only_when_more_rows_exist() {
        let page = RequestListPage::from_query(Some(50), Some(100));

        assert_eq!(page.next_offset(true), Some(150));
        assert_eq!(page.next_offset(false), None);
    }

    #[test]
    fn request_facet_window_uses_safe_defaults() {
        let window = RequestFacetWindow::from_query(None, None);

        assert_eq!(
            window,
            RequestFacetWindow {
                since_hours: DEFAULT_REQUEST_FACET_SINCE_HOURS,
                limit: DEFAULT_REQUEST_FACET_LIMIT,
            }
        );
    }

    #[test]
    fn request_facet_window_clamps_bounds() {
        let max = RequestFacetWindow::from_query(Some(i64::MAX), Some(i64::MAX));
        assert_eq!(max.since_hours, MAX_REQUEST_FACET_SINCE_HOURS);
        assert_eq!(max.limit, MAX_REQUEST_FACET_LIMIT);

        let min = RequestFacetWindow::from_query(Some(-10), Some(-10));
        assert_eq!(min.since_hours, 1);
        assert_eq!(min.limit, 1);
    }

    #[test]
    fn request_facet_window_calculates_cutoff() {
        let now = DateTime::parse_from_rfc3339("2026-07-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let window = RequestFacetWindow::from_query(Some(6), Some(10));

        assert_eq!(
            window.cutoff(now),
            DateTime::parse_from_rfc3339("2026-07-01T06:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
        );
    }

    #[test]
    fn recent_error_window_uses_safe_defaults() {
        let window = RecentErrorWindow::from_query(None, None);

        assert_eq!(
            window,
            RecentErrorWindow {
                since_hours: DEFAULT_RECENT_ERROR_SINCE_HOURS,
                limit: DEFAULT_RECENT_ERROR_LIMIT,
            }
        );
        assert_eq!(window.fetch_limit(), DEFAULT_RECENT_ERROR_LIMIT + 1);
    }

    #[test]
    fn recent_error_window_clamps_bounds() {
        let max = RecentErrorWindow::from_query(Some(i64::MAX), Some(i64::MAX));
        assert_eq!(max.since_hours, MAX_RECENT_ERROR_SINCE_HOURS);
        assert_eq!(max.limit, MAX_RECENT_ERROR_LIMIT);

        let min = RecentErrorWindow::from_query(Some(-10), Some(-10));
        assert_eq!(min.since_hours, 1);
        assert_eq!(min.limit, 1);
        assert_eq!(min.fetch_limit(), 2);
    }

    #[test]
    fn recent_error_window_calculates_cutoff() {
        let now = DateTime::parse_from_rfc3339("2026-07-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let window = RecentErrorWindow::from_query(Some(12), Some(25));

        assert_eq!(
            window.cutoff(now),
            DateTime::parse_from_rfc3339("2026-07-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
        );
    }

    #[test]
    fn session_list_page_uses_safe_defaults() {
        let page = SessionListPage::from_query(None, None);

        assert_eq!(
            page,
            SessionListPage {
                limit: DEFAULT_SESSION_LIST_LIMIT,
                offset: 0,
            }
        );
        assert_eq!(page.fetch_limit(), DEFAULT_SESSION_LIST_LIMIT + 1);
    }

    #[test]
    fn session_list_page_clamps_limit_and_offset() {
        let page = SessionListPage::from_query(Some(i64::MAX), Some(i64::MAX));

        assert_eq!(page.limit, MAX_SESSION_LIST_LIMIT);
        assert_eq!(page.offset, MAX_SESSION_LIST_OFFSET);
    }

    #[test]
    fn session_list_page_clamps_negative_values() {
        let page = SessionListPage::from_query(Some(-10), Some(-10));

        assert_eq!(page.limit, 1);
        assert_eq!(page.offset, 0);
    }

    #[test]
    fn session_list_page_reports_next_offset_only_when_more_rows_exist() {
        let page = SessionListPage::from_query(Some(50), Some(100));

        assert_eq!(page.next_offset(true), Some(150));
        assert_eq!(page.next_offset(false), None);
    }

    #[test]
    fn session_message_page_uses_safe_defaults() {
        let page = SessionMessagePage::from_query(None, None);

        assert_eq!(
            page,
            SessionMessagePage {
                limit: DEFAULT_SESSION_MESSAGE_LIMIT,
                offset: 0,
            }
        );
        assert_eq!(page.fetch_limit(), DEFAULT_SESSION_MESSAGE_LIMIT + 1);
    }

    #[test]
    fn session_message_page_clamps_limit_and_offset() {
        let page = SessionMessagePage::from_query(Some(i64::MAX), Some(i64::MAX));

        assert_eq!(page.limit, MAX_SESSION_MESSAGE_LIMIT);
        assert_eq!(page.offset, MAX_SESSION_MESSAGE_OFFSET);
    }

    #[test]
    fn session_message_page_clamps_negative_values() {
        let page = SessionMessagePage::from_query(Some(-10), Some(-10));

        assert_eq!(page.limit, 1);
        assert_eq!(page.offset, 0);
    }

    #[test]
    fn session_message_page_reports_next_offset_only_when_more_rows_exist() {
        let page = SessionMessagePage::from_query(Some(50), Some(100));

        assert_eq!(page.next_offset(true), Some(150));
        assert_eq!(page.next_offset(false), None);
    }

    #[test]
    fn audit_event_page_uses_safe_defaults() {
        let page = AuditEventPage::from_query(None, None);

        assert_eq!(
            page,
            AuditEventPage {
                limit: DEFAULT_AUDIT_EVENT_LIMIT,
                offset: 0,
            }
        );
        assert_eq!(page.fetch_limit(), DEFAULT_AUDIT_EVENT_LIMIT + 1);
    }

    #[test]
    fn audit_event_page_clamps_limit_and_offset() {
        let page = AuditEventPage::from_query(Some(i64::MAX), Some(i64::MAX));

        assert_eq!(page.limit, MAX_AUDIT_EVENT_LIMIT);
        assert_eq!(page.offset, MAX_AUDIT_EVENT_OFFSET);
    }

    #[test]
    fn audit_event_page_clamps_negative_values() {
        let page = AuditEventPage::from_query(Some(-10), Some(-10));

        assert_eq!(page.limit, 1);
        assert_eq!(page.offset, 0);
    }

    #[test]
    fn audit_event_page_reports_next_offset_only_when_more_rows_exist() {
        let page = AuditEventPage::from_query(Some(50), Some(100));

        assert_eq!(page.next_offset(true), Some(150));
        assert_eq!(page.next_offset(false), None);
    }

    #[test]
    fn usage_summary_window_uses_safe_defaults() {
        let window = UsageSummaryWindow::from_query(None, None);

        assert_eq!(
            window,
            UsageSummaryWindow {
                since_hours: DEFAULT_USAGE_SUMMARY_SINCE_HOURS,
                limit: DEFAULT_USAGE_SUMMARY_LIMIT,
            }
        );
    }

    #[test]
    fn usage_summary_window_clamps_bounds() {
        let max = UsageSummaryWindow::from_query(Some(i64::MAX), Some(i64::MAX));
        assert_eq!(max.since_hours, MAX_USAGE_SUMMARY_SINCE_HOURS);
        assert_eq!(max.limit, MAX_USAGE_SUMMARY_LIMIT);

        let min = UsageSummaryWindow::from_query(Some(-10), Some(-10));
        assert_eq!(min.since_hours, 1);
        assert_eq!(min.limit, 1);
    }

    #[test]
    fn usage_summary_window_calculates_cutoff() {
        let now = DateTime::parse_from_rfc3339("2026-07-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let window = UsageSummaryWindow::from_query(Some(6), Some(10));

        assert_eq!(
            window.cutoff(now),
            DateTime::parse_from_rfc3339("2026-07-01T06:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
        );
    }

    #[test]
    fn usage_timeseries_window_uses_safe_defaults() {
        let window = UsageTimeseriesWindow::from_query(None, None).unwrap();

        assert_eq!(
            window,
            UsageTimeseriesWindow {
                since_hours: DEFAULT_USAGE_TIMESERIES_SINCE_HOURS,
                bucket: UsageTimeseriesBucket::Hour,
            }
        );
    }

    #[test]
    fn usage_timeseries_window_clamps_by_bucket() {
        let minute = UsageTimeseriesWindow::from_query(Some(i64::MAX), Some("minute")).unwrap();
        assert_eq!(minute.since_hours, MAX_USAGE_TIMESERIES_MINUTE_HOURS);
        assert_eq!(minute.bucket, UsageTimeseriesBucket::Minute);

        let hour = UsageTimeseriesWindow::from_query(Some(i64::MAX), Some("hour")).unwrap();
        assert_eq!(hour.since_hours, MAX_USAGE_TIMESERIES_HOUR_HOURS);

        let day = UsageTimeseriesWindow::from_query(Some(i64::MAX), Some("day")).unwrap();
        assert_eq!(day.since_hours, MAX_USAGE_TIMESERIES_DAY_HOURS);

        let min = UsageTimeseriesWindow::from_query(Some(-10), Some("day")).unwrap();
        assert_eq!(min.since_hours, 1);
    }

    #[test]
    fn usage_timeseries_window_rejects_unknown_bucket() {
        let error = UsageTimeseriesWindow::from_query(Some(24), Some("week"))
            .unwrap_err()
            .to_string();

        assert!(error.contains("bucket must be one of"));
    }

    #[test]
    fn usage_timeseries_window_calculates_cutoff() {
        let now = DateTime::parse_from_rfc3339("2026-07-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let window = UsageTimeseriesWindow::from_query(Some(12), Some("hour")).unwrap();

        assert_eq!(
            window.cutoff(now),
            DateTime::parse_from_rfc3339("2026-07-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
        );
    }

    #[test]
    fn retention_cutoff_subtracts_retention_days() {
        let now = DateTime::parse_from_rfc3339("2026-06-29T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        assert_eq!(
            retention_cutoff(now, 30),
            DateTime::parse_from_rfc3339("2026-05-30T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
        );
    }

    #[test]
    fn retention_prune_result_sums_deleted_rows() {
        let result = RetentionPruneResult {
            request_traces: 1,
            trace_rollups_minute: 2,
            trace_sessions: 3,
            ui_audit_events: 4,
            ui_sessions: 5,
            oauth_states: 6,
        };

        assert_eq!(result.total_deleted(), 21);
    }

    #[test]
    fn list_sessions_query_limits_sessions_before_request_stats() {
        let selected_sessions = LIST_SESSIONS_SQL.find("WITH selected_sessions").unwrap();
        let session_limit = LIST_SESSIONS_SQL.find("LIMIT $2 OFFSET $3").unwrap();
        let lateral_stats = LIST_SESSIONS_SQL.find("LEFT JOIN LATERAL").unwrap();

        assert!(selected_sessions < session_limit);
        assert!(session_limit < lateral_stats);
        assert!(LIST_SESSIONS_SQL.contains("$1::text IS NULL"));
        assert!(LIST_SESSIONS_SQL.contains("session_key ILIKE '%' || $1 || '%' ESCAPE '\\'"));
        assert!(LIST_SESSIONS_SQL.contains("user_id ILIKE '%' || $1 || '%' ESCAPE '\\'"));
        assert!(LIST_SESSIONS_SQL.contains("user_name ILIKE '%' || $1 || '%' ESCAPE '\\'"));
        assert!(LIST_SESSIONS_SQL.contains("WHERE r.session_id = s.id"));
    }

    #[test]
    fn list_requests_query_filters_then_pages_requests() {
        let where_clause = LIST_REQUESTS_SQL.find("WHERE (").unwrap();
        let request_limit = LIST_REQUESTS_SQL.find("LIMIT $13 OFFSET $14").unwrap();

        assert!(where_clause < request_limit);
        assert!(LIST_REQUESTS_SQL.contains("$1::text IS NULL"));
        assert!(LIST_REQUESTS_SQL.contains("upstream_url ILIKE '%' || $1 || '%' ESCAPE '\\'"));
        assert!(LIST_REQUESTS_SQL.contains("model ILIKE '%' || $1 || '%' ESCAPE '\\'"));
        assert!(LIST_REQUESTS_SQL.contains("request_kind ILIKE '%' || $1 || '%' ESCAPE '\\'"));
        assert!(LIST_REQUESTS_SQL.contains("AND ($2::int IS NULL OR status = $2)"));
        assert!(LIST_REQUESTS_SQL.contains("AND ($3::text IS NULL OR upstream_host = $3)"));
        assert!(LIST_REQUESTS_SQL.contains("AND ($4::text IS NULL OR model = $4)"));
        assert!(LIST_REQUESTS_SQL.contains("AND ($5::text IS NULL OR request_kind = $5)"));
        assert!(LIST_REQUESTS_SQL.contains("AND ($6::uuid IS NULL OR session_id = $6)"));
        assert!(LIST_REQUESTS_SQL.contains("AND ($7::timestamptz IS NULL OR started_at >= $7)"));
        assert!(LIST_REQUESTS_SQL.contains("AND ($8::timestamptz IS NULL OR started_at <= $8)"));
        assert!(LIST_REQUESTS_SQL.contains("AND ($9::bigint IS NULL OR duration_ms >= $9)"));
        assert!(LIST_REQUESTS_SQL.contains("AND ($10::bigint IS NULL OR duration_ms <= $10)"));
        assert!(LIST_REQUESTS_SQL.contains("$11::boolean IS NULL"));
        assert!(
            LIST_REQUESTS_SQL.contains("OR ($11 = true AND (error IS NOT NULL OR status >= 500))")
        );
        assert!(
            LIST_REQUESTS_SQL.contains(
                "OR ($11 = false AND error IS NULL AND (status IS NULL OR status < 500))"
            )
        );
        assert!(LIST_REQUESTS_SQL.contains("$12::text IS NULL"));
        assert!(LIST_REQUESTS_SQL.contains("OR ($12 = 'no_status' AND status IS NULL)"));
        assert!(LIST_REQUESTS_SQL.contains("OR ($12 = '5xx' AND status BETWEEN 500 AND 599)"));
        assert!(LIST_REQUESTS_SQL.contains(
            "OR ($12 = 'other' AND status IS NOT NULL AND (status < 100 OR status > 599))"
        ));
        assert!(LIST_REQUESTS_SQL.contains("ORDER BY started_at DESC, id DESC"));
    }

    #[test]
    fn list_session_requests_query_filters_by_session_before_paging() {
        let session_filter = LIST_SESSION_REQUESTS_SQL
            .find("WHERE session_id = $1")
            .unwrap();
        let request_order = LIST_SESSION_REQUESTS_SQL
            .find("ORDER BY started_at DESC, id DESC")
            .unwrap();
        let request_limit = LIST_SESSION_REQUESTS_SQL
            .find("LIMIT $2 OFFSET $3")
            .unwrap();

        assert!(session_filter < request_order);
        assert!(request_order < request_limit);
        assert!(LIST_SESSION_REQUESTS_SQL.contains("FROM trace_requests"));
        assert!(LIST_SESSION_REQUESTS_SQL.contains("plugin_metadata, tags"));
    }

    #[test]
    fn recent_error_requests_query_filters_errors_and_orders_latest() {
        let cutoff_filter = RECENT_ERROR_REQUESTS_SQL
            .find("WHERE started_at >= $1")
            .unwrap();
        let error_filter = RECENT_ERROR_REQUESTS_SQL
            .find("AND (error IS NOT NULL OR status >= 500)")
            .unwrap();
        let request_order = RECENT_ERROR_REQUESTS_SQL
            .find("ORDER BY started_at DESC, id DESC")
            .unwrap();
        let request_limit = RECENT_ERROR_REQUESTS_SQL.find("LIMIT $2").unwrap();

        assert!(cutoff_filter < error_filter);
        assert!(error_filter < request_order);
        assert!(request_order < request_limit);
        assert!(RECENT_ERROR_REQUESTS_SQL.contains("FROM trace_requests"));
        assert!(RECENT_ERROR_REQUESTS_SQL.contains("plugin_metadata, tags"));
    }

    #[test]
    fn session_request_stats_query_uses_session_scope_and_error_metrics() {
        assert!(SESSION_REQUEST_STATS_SQL.contains("FROM request_traces"));
        assert!(SESSION_REQUEST_STATS_SQL.contains("WHERE session_id = $1"));
        assert!(SESSION_REQUEST_STATS_SQL.contains(
            "COUNT(*) FILTER (WHERE error IS NOT NULL OR status >= 500)::bigint AS error_count"
        ));
        assert!(SESSION_REQUEST_STATS_SQL.contains(
            "COALESCE(SUM(request_body_bytes + response_body_bytes), 0)::bigint AS captured_bytes"
        ));
        assert!(SESSION_REQUEST_STATS_SQL.contains("AVG(duration_ms)::bigint AS avg_duration_ms"));
        assert!(SESSION_REQUEST_STATS_SQL.contains("MAX(duration_ms)::bigint AS max_duration_ms"));
        assert!(SESSION_REQUEST_STATS_SQL.contains("AVG(ttft_ms)::bigint AS avg_ttft_ms"));
        assert!(SESSION_REQUEST_STATS_SQL.contains("MAX(ttft_ms)::bigint AS max_ttft_ms"));
        assert!(SESSION_REQUEST_STATS_SQL.contains("MIN(started_at) AS first_request_at"));
        assert!(SESSION_REQUEST_STATS_SQL.contains("MAX(started_at) AS last_request_at"));
    }

    #[test]
    fn request_facet_queries_are_bounded_and_filter_empty_values() {
        for query in [
            REQUEST_FACET_MODELS_SQL,
            REQUEST_FACET_UPSTREAM_HOSTS_SQL,
            REQUEST_FACET_REQUEST_KINDS_SQL,
        ] {
            assert!(query.contains("WHERE started_at >= $1"));
            assert!(query.contains("IS NOT NULL"));
            assert!(query.contains("<> ''"));
            assert!(query.contains("ORDER BY request_count DESC, value ASC"));
            assert!(query.contains("LIMIT $2"));
        }

        assert!(REQUEST_FACET_STATUSES_SQL.contains("WHERE started_at >= $1"));
        assert!(REQUEST_FACET_STATUSES_SQL.contains("status IS NOT NULL"));
        assert!(REQUEST_FACET_STATUSES_SQL.contains("LIMIT $2"));
        assert!(REQUEST_FACET_STATUS_CLASSES_SQL.contains("WHEN status IS NULL THEN 'no_status'"));
        assert!(
            REQUEST_FACET_STATUS_CLASSES_SQL.contains("WHEN status BETWEEN 500 AND 599 THEN '5xx'")
        );
        assert!(REQUEST_FACET_STATUS_CLASSES_SQL.contains("ELSE 'other'"));
        assert!(REQUEST_FACET_STATUS_CLASSES_SQL.contains("WHERE started_at >= $1"));
        assert!(REQUEST_FACET_STATUS_CLASSES_SQL.contains("LIMIT $2"));
        assert!(REQUEST_FACET_ERROR_STATES_SQL.contains("WHERE started_at >= $1"));
        assert!(
            REQUEST_FACET_ERROR_STATES_SQL
                .contains("error IS NOT NULL OR COALESCE(status >= 500, false)")
        );
        assert!(REQUEST_FACET_ERROR_STATES_SQL.contains("GROUP BY 1"));
    }

    #[test]
    fn request_search_escapes_like_wildcards() {
        assert_eq!(escape_like(r#"100%\_match"#), r#"100\%\\\_match"#);
    }

    #[tokio::test]
    async fn readiness_check_timeout_allows_fast_future() {
        readiness_check_with_timeout(async { Ok(()) }, Duration::from_millis(1))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn readiness_check_timeout_rejects_slow_future() {
        let error = readiness_check_with_timeout(
            async { std::future::pending::<anyhow::Result<()>>().await },
            Duration::from_millis(1),
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(error.contains("readiness check timed out"));
    }

    #[test]
    fn structured_query_rejects_unknown_field() {
        let dataset = dataset_spec("requests").unwrap();
        let fields = vec!["id".to_string(), "request_body_compressed".to_string()];

        let error = selected_fields(dataset, Some(&fields))
            .unwrap_err()
            .to_string();

        assert!(error.contains("is not allowed"));
    }

    #[test]
    fn structured_query_rejects_too_many_selected_fields() {
        let dataset = dataset_spec("requests").unwrap();
        let fields = vec!["id".to_string(); MAX_STRUCTURED_QUERY_FIELDS + 1];

        let error = selected_fields(dataset, Some(&fields))
            .unwrap_err()
            .to_string();

        assert!(error.contains("fields must contain at most"));
    }

    #[test]
    fn structured_query_deduplicates_selected_fields() {
        let dataset = dataset_spec("requests").unwrap();
        let fields = vec!["id".to_string(), "id".to_string(), "status".to_string()];

        let selected = selected_fields(dataset, Some(&fields)).unwrap();

        assert_eq!(
            selected
                .iter()
                .map(|field| field.name())
                .collect::<Vec<_>>(),
            vec!["id", "status"]
        );
    }

    #[test]
    fn structured_query_allows_plugin_metadata_path_field() {
        let dataset = dataset_spec("requests").unwrap();
        let fields = vec![
            "id".to_string(),
            "plugin_metadata.api-key-user-mapper.customer_tier".to_string(),
        ];

        let selected = selected_fields(dataset, Some(&fields)).unwrap();

        assert_eq!(
            selected
                .iter()
                .map(|field| field.name())
                .collect::<Vec<_>>(),
            vec!["id", "plugin_metadata.api-key-user-mapper.customer_tier"]
        );
    }

    #[test]
    fn structured_query_rejects_invalid_plugin_metadata_path_field() {
        let dataset = dataset_spec("requests").unwrap();
        let fields = vec!["plugin_metadata.api.key with spaces".to_string()];

        let error = selected_fields(dataset, Some(&fields))
            .unwrap_err()
            .to_string();

        assert!(error.contains("plugin_metadata path segment"));
    }

    #[test]
    fn structured_query_rejects_too_many_filters() {
        let filters = vec![
            QueryFilter {
                field: "status".to_string(),
                op: QueryOp::Eq,
                value: Some(json!(200)),
            };
            MAX_STRUCTURED_QUERY_FILTERS + 1
        ];

        let error = validate_structured_query_filters(&filters)
            .unwrap_err()
            .to_string();

        assert!(error.contains("filters must contain at most"));
    }

    #[test]
    fn structured_query_rejects_too_many_order_fields() {
        let dataset = dataset_spec("requests").unwrap();
        let order_by = vec![
            QueryOrder {
                field: "started_at".to_string(),
                direction: SortDirection::Desc,
            };
            MAX_STRUCTURED_QUERY_ORDER_BY + 1
        ];

        let error = selected_order(dataset, &order_by).unwrap_err().to_string();

        assert!(error.contains("order_by must contain at most"));
    }

    #[test]
    fn structured_query_rejects_wrong_filter_type() {
        let dataset = dataset_spec("requests").unwrap();
        let filter = QueryFilter {
            field: "status".to_string(),
            op: QueryOp::Eq,
            value: Some(json!("200")),
        };
        let mut builder = QueryBuilder::<Postgres>::new("");

        let error = append_filter(&mut builder, dataset, &filter)
            .unwrap_err()
            .to_string();

        assert!(error.contains("requires an integer value"));
    }

    #[test]
    fn structured_query_rejects_oversized_string_filter_value() {
        let dataset = dataset_spec("requests").unwrap();
        let filter = QueryFilter {
            field: "model".to_string(),
            op: QueryOp::Eq,
            value: Some(json!(
                "a".repeat(MAX_STRUCTURED_QUERY_STRING_VALUE_BYTES + 1)
            )),
        };
        let mut builder = QueryBuilder::<Postgres>::new("");

        let error = append_filter(&mut builder, dataset, &filter)
            .unwrap_err()
            .to_string();

        assert!(error.contains("string value must be at most"));
    }

    #[test]
    fn structured_query_rejects_oversized_json_filter_value() {
        let dataset = dataset_spec("requests").unwrap();
        let filter = QueryFilter {
            field: "plugin_metadata".to_string(),
            op: QueryOp::Contains,
            value: Some(json!({"payload": "a".repeat(MAX_STRUCTURED_QUERY_JSON_VALUE_BYTES)})),
        };
        let mut builder = QueryBuilder::<Postgres>::new("");

        let error = append_filter(&mut builder, dataset, &filter)
            .unwrap_err()
            .to_string();

        assert!(error.contains("JSON value must be at most"));
    }

    #[test]
    fn structured_query_allows_tag_membership_filter() {
        let dataset = dataset_spec("requests").unwrap();
        let filter = QueryFilter {
            field: "tags".to_string(),
            op: QueryOp::Contains,
            value: Some(json!("websocket")),
        };
        let mut builder = QueryBuilder::<Postgres>::new("");

        append_filter(&mut builder, dataset, &filter).unwrap();
    }

    #[test]
    fn structured_query_allows_plugin_metadata_path_filter() {
        let dataset = dataset_spec("requests").unwrap();
        let filter = QueryFilter {
            field: "plugin_metadata.api-key-user-mapper.customer_tier".to_string(),
            op: QueryOp::Eq,
            value: Some(json!("enterprise")),
        };
        let mut builder = QueryBuilder::<Postgres>::new("");

        append_filter(&mut builder, dataset, &filter).unwrap();
    }
}
