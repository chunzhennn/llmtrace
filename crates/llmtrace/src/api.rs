use axum::extract::{Path, Query, State};
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::state::AppState;
use crate::storage;

const MAX_REQUEST_SEARCH_BYTES: usize = 512;
const MAX_REQUEST_FILTER_BYTES: usize = 1024;
const MAX_REQUEST_TIME_FILTER_BYTES: usize = 128;
const MAX_AUDIT_FILTER_BYTES: usize = 1024;
const JSONL_CONTENT_TYPE: &str = "application/x-ndjson; charset=utf-8";
const EXPORT_ROWS_HEADER: HeaderName = HeaderName::from_static("x-llmtrace-export-rows");
const REQUEST_KIND_FILTERS: &[&str] = &[
    "openai_chat_completions",
    "openai_responses",
    "anthropic_messages",
    "websocket",
    "generic_json",
    "generic_http",
];
const STATUS_CLASS_FILTERS: &[&str] = &["no_status", "1xx", "2xx", "3xx", "4xx", "5xx", "other"];
const USAGE_TIMESERIES_BUCKETS: &[&str] = &["minute", "hour", "day"];

#[derive(Debug, Deserialize)]
struct RequestListQuery {
    q: Option<String>,
    status: Option<i32>,
    status_class: Option<String>,
    has_error: Option<bool>,
    upstream_host: Option<String>,
    model: Option<String>,
    request_kind: Option<String>,
    session_id: Option<String>,
    since: Option<String>,
    until: Option<String>,
    min_duration_ms: Option<i64>,
    max_duration_ms: Option<i64>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct RequestFacetQuery {
    since_hours: Option<i64>,
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct SessionListQuery {
    q: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct SessionDetailQuery {
    messages_limit: Option<i64>,
    messages_offset: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct SessionRequestListQuery {
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct AuditEventListQuery {
    event_type: Option<String>,
    user_id: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct UsageSummaryQuery {
    since_hours: Option<i64>,
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct UsageTimeseriesQuery {
    since_hours: Option<i64>,
    bucket: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RequestTimeRange {
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RequestDurationRange {
    min_duration_ms: Option<i64>,
    max_duration_ms: Option<i64>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/stats", get(stats))
        .route("/usage/summary", get(usage_summary))
        .route("/usage/timeseries", get(usage_timeseries))
        .route("/requests", get(list_requests))
        .route("/requests/facets", get(request_facets))
        .route("/requests/{id}", get(get_request))
        .route("/sessions", get(list_sessions))
        .route("/sessions/{id}/requests", get(list_session_requests))
        .route("/sessions/{id}", get(get_session))
        .route("/audit-events", get(list_audit_events))
        .route("/query", post(run_query))
        .route("/query/schema", get(query_schema))
        .route("/query/export.jsonl", post(export_query_jsonl))
        .route("/plugins", get(plugins))
}

async fn stats(State(state): State<AppState>) -> Response {
    match storage::stats(&state.pool).await {
        Ok(mut value) => {
            if let Some(object) = value.as_object_mut() {
                object.insert(
                    "runtime".to_string(),
                    state.runtime_metrics.snapshot(state.traces.queue_metrics()),
                );
            }
            Json(value).into_response()
        }
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn usage_summary(
    State(state): State<AppState>,
    Query(query): Query<UsageSummaryQuery>,
) -> Response {
    match storage::usage_summary(&state.pool, query.since_hours, query.limit).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn usage_timeseries(
    State(state): State<AppState>,
    Query(query): Query<UsageTimeseriesQuery>,
) -> Response {
    let bucket = match normalize_usage_timeseries_bucket(query.bucket) {
        Ok(bucket) => bucket,
        Err(message) => return bad_request(message),
    };

    match storage::usage_timeseries(&state.pool, query.since_hours, bucket.as_deref()).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn list_requests(
    State(state): State<AppState>,
    Query(query): Query<RequestListQuery>,
) -> Response {
    let q = match normalize_request_search(query.q) {
        Ok(q) => q,
        Err(message) => return bad_request(message),
    };
    let time_range = match normalize_request_time_range(query.since, query.until) {
        Ok(range) => range,
        Err(message) => return bad_request(message),
    };
    let duration_range =
        match normalize_request_duration_range(query.min_duration_ms, query.max_duration_ms) {
            Ok(range) => range,
            Err(message) => return bad_request(message),
        };
    let upstream_host = match normalize_request_filter("upstream_host", query.upstream_host) {
        Ok(value) => value,
        Err(message) => return bad_request(message),
    };
    let model = match normalize_request_filter("model", query.model) {
        Ok(value) => value,
        Err(message) => return bad_request(message),
    };
    let request_kind = match normalize_request_kind_filter(query.request_kind) {
        Ok(value) => value,
        Err(message) => return bad_request(message),
    };
    let session_id = match normalize_optional_uuid_filter("session_id", query.session_id) {
        Ok(value) => value,
        Err(message) => return bad_request(message),
    };
    let status_class = match normalize_status_class_filter(query.status_class) {
        Ok(value) => value,
        Err(message) => return bad_request(message),
    };

    match storage::list_requests(
        &state.pool,
        storage::RequestListFilters {
            q,
            status: query.status,
            status_class,
            has_error: query.has_error,
            upstream_host,
            model,
            request_kind,
            session_id,
            since: time_range.since,
            until: time_range.until,
            min_duration_ms: duration_range.min_duration_ms,
            max_duration_ms: duration_range.max_duration_ms,
            limit: query.limit,
            offset: query.offset,
        },
    )
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn request_facets(
    State(state): State<AppState>,
    Query(query): Query<RequestFacetQuery>,
) -> Response {
    match storage::request_facets(&state.pool, query.since_hours, query.limit).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn get_request(State(state): State<AppState>, Path(id): Path<Uuid>) -> Response {
    match storage::get_request(&state.pool, id, state.config.proxy.max_body_capture_bytes).await {
        Ok(Some(value)) => Json(value).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "request not found"})),
        )
            .into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn list_sessions(
    State(state): State<AppState>,
    Query(query): Query<SessionListQuery>,
) -> Response {
    let q = match normalize_request_search(query.q) {
        Ok(q) => q,
        Err(message) => return bad_request(message),
    };

    match storage::list_sessions(&state.pool, q, query.limit, query.offset).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn get_session(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<SessionDetailQuery>,
) -> Response {
    match storage::get_session(&state.pool, id, query.messages_limit, query.messages_offset).await {
        Ok(Some(value)) => Json(value).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "session not found"})),
        )
            .into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn list_session_requests(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<SessionRequestListQuery>,
) -> Response {
    match storage::list_session_requests(&state.pool, id, query.limit, query.offset).await {
        Ok(Some(value)) => Json(value).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "session not found"})),
        )
            .into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn list_audit_events(
    State(state): State<AppState>,
    Query(query): Query<AuditEventListQuery>,
) -> Response {
    let event_type = match normalize_optional_filter("event_type", query.event_type) {
        Ok(value) => value,
        Err(message) => return bad_request(message),
    };
    let user_id = match normalize_optional_filter("user_id", query.user_id) {
        Ok(value) => value,
        Err(message) => return bad_request(message),
    };

    match storage::list_audit_events(&state.pool, event_type, user_id, query.limit, query.offset)
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn run_query(
    State(state): State<AppState>,
    Json(payload): Json<storage::StructuredQuery>,
) -> Response {
    match storage::run_structured_query(&state.pool, payload).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => structured_query_error(error),
    }
}

async fn query_schema() -> Response {
    Json(storage::structured_query_schema()).into_response()
}

async fn export_query_jsonl(
    State(state): State<AppState>,
    Json(payload): Json<storage::StructuredQuery>,
) -> Response {
    match storage::run_structured_query(&state.pool, payload).await {
        Ok(value) => structured_query_jsonl_response(value),
        Err(error) => structured_query_error(error),
    }
}

async fn plugins(State(state): State<AppState>) -> Response {
    Json(json!({ "items": state.plugins.statuses() })).into_response()
}

fn bad_request(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": message.into()})),
    )
        .into_response()
}

fn api_error(status: StatusCode, error: anyhow::Error) -> Response {
    if status.is_server_error() {
        tracing::error!(%error, %status, "api request failed");
        return (status, Json(json!({"error": "internal server error"}))).into_response();
    }
    (status, Json(json!({"error": error.to_string()}))).into_response()
}

fn structured_query_error(error: storage::StructuredQueryError) -> Response {
    match error {
        storage::StructuredQueryError::Invalid(message) => bad_request(message),
        storage::StructuredQueryError::Execution(error) => {
            api_error(StatusCode::INTERNAL_SERVER_ERROR, error)
        }
    }
}

fn structured_query_jsonl_response(value: Value) -> Response {
    let Some(rows) = value.get("rows").and_then(Value::as_array) else {
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            anyhow::anyhow!("structured query result did not contain a row array"),
        );
    };
    let dataset = value
        .get("dataset")
        .and_then(Value::as_str)
        .unwrap_or("query");

    let mut body = String::new();
    for row in rows {
        match serde_json::to_string(row) {
            Ok(line) => {
                body.push_str(&line);
                body.push('\n');
            }
            Err(error) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, error.into()),
        }
    }

    let mut response = (StatusCode::OK, body).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(JSONL_CONTENT_TYPE),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!(
            "attachment; filename=\"llmtrace-{dataset}.jsonl\""
        ))
        .unwrap_or_else(|_| {
            HeaderValue::from_static("attachment; filename=\"llmtrace-query.jsonl\"")
        }),
    );
    headers.insert(
        EXPORT_ROWS_HEADER,
        HeaderValue::from_str(&rows.len().to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
    response
}

fn normalize_request_search(q: Option<String>) -> Result<Option<String>, String> {
    let Some(q) = q else {
        return Ok(None);
    };
    let q = q.trim();
    if q.is_empty() {
        return Ok(None);
    }
    if q.len() > MAX_REQUEST_SEARCH_BYTES {
        return Err(format!(
            "q must be at most {MAX_REQUEST_SEARCH_BYTES} bytes"
        ));
    }
    Ok(Some(q.to_string()))
}

fn normalize_request_filter(field: &str, value: Option<String>) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > MAX_REQUEST_FILTER_BYTES {
        return Err(format!(
            "{field} must be at most {MAX_REQUEST_FILTER_BYTES} bytes"
        ));
    }
    Ok(Some(value.to_string()))
}

fn normalize_request_kind_filter(value: Option<String>) -> Result<Option<String>, String> {
    let Some(value) = normalize_request_filter("request_kind", value)? else {
        return Ok(None);
    };
    if REQUEST_KIND_FILTERS.contains(&value.as_str()) {
        return Ok(Some(value));
    }
    Err(format!(
        "request_kind must be one of {}",
        REQUEST_KIND_FILTERS.join(", ")
    ))
}

fn normalize_status_class_filter(value: Option<String>) -> Result<Option<String>, String> {
    let Some(value) = normalize_request_filter("status_class", value)? else {
        return Ok(None);
    };
    let value = value.to_ascii_lowercase();
    if STATUS_CLASS_FILTERS.contains(&value.as_str()) {
        return Ok(Some(value));
    }
    Err(format!(
        "status_class must be one of {}",
        STATUS_CLASS_FILTERS.join(", ")
    ))
}

fn normalize_optional_uuid_filter(
    field: &str,
    value: Option<String>,
) -> Result<Option<Uuid>, String> {
    let Some(value) = normalize_request_filter(field, value)? else {
        return Ok(None);
    };
    Uuid::parse_str(&value)
        .map(Some)
        .map_err(|_| format!("{field} must be a UUID"))
}

fn normalize_request_time_range(
    since: Option<String>,
    until: Option<String>,
) -> Result<RequestTimeRange, String> {
    let since = normalize_optional_timestamp_filter("since", since)?;
    let until = normalize_optional_timestamp_filter("until", until)?;
    if since.zip(until).is_some_and(|(since, until)| since > until) {
        return Err("since must be earlier than or equal to until".to_string());
    }
    Ok(RequestTimeRange { since, until })
}

fn normalize_request_duration_range(
    min_duration_ms: Option<i64>,
    max_duration_ms: Option<i64>,
) -> Result<RequestDurationRange, String> {
    let min_duration_ms = normalize_optional_duration_filter("min_duration_ms", min_duration_ms)?;
    let max_duration_ms = normalize_optional_duration_filter("max_duration_ms", max_duration_ms)?;
    if min_duration_ms
        .zip(max_duration_ms)
        .is_some_and(|(min, max)| min > max)
    {
        return Err("min_duration_ms must be less than or equal to max_duration_ms".to_string());
    }
    Ok(RequestDurationRange {
        min_duration_ms,
        max_duration_ms,
    })
}

fn normalize_optional_duration_filter(
    field: &str,
    value: Option<i64>,
) -> Result<Option<i64>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value < 0 {
        return Err(format!("{field} must be greater than or equal to 0"));
    }
    Ok(Some(value))
}

fn normalize_optional_timestamp_filter(
    field: &str,
    value: Option<String>,
) -> Result<Option<DateTime<Utc>>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > MAX_REQUEST_TIME_FILTER_BYTES {
        return Err(format!(
            "{field} must be at most {MAX_REQUEST_TIME_FILTER_BYTES} bytes"
        ));
    }
    Ok(Some(
        DateTime::parse_from_rfc3339(value)
            .map_err(|_| format!("{field} must be an RFC3339 timestamp"))?
            .with_timezone(&Utc),
    ))
}

fn normalize_optional_filter(field: &str, value: Option<String>) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > MAX_AUDIT_FILTER_BYTES {
        return Err(format!(
            "{field} must be at most {MAX_AUDIT_FILTER_BYTES} bytes"
        ));
    }
    Ok(Some(value.to_string()))
}

fn normalize_usage_timeseries_bucket(value: Option<String>) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let normalized = value.to_ascii_lowercase();
    if USAGE_TIMESERIES_BUCKETS.contains(&normalized.as_str()) {
        return Ok(Some(normalized));
    }
    Err("bucket must be one of minute, hour, or day".to_string())
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;

    use super::*;

    #[tokio::test]
    async fn structured_query_validation_error_returns_bad_request_message() {
        let response = structured_query_error(storage::StructuredQueryError::Invalid(
            "unknown query dataset secret_table".to_string(),
        ));
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = response_body_json(response).await;

        assert_eq!(body, json!({"error": "unknown query dataset secret_table"}));
    }

    #[tokio::test]
    async fn structured_query_execution_error_returns_generic_server_error() {
        let response = structured_query_error(storage::StructuredQueryError::Execution(
            anyhow::anyhow!("database host=internal.example timed out"),
        ));
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body = response_body_json(response).await;

        assert_eq!(body, json!({"error": "internal server error"}));
    }

    #[tokio::test]
    async fn query_schema_returns_structured_query_schema() {
        let response = query_schema().await;
        assert_eq!(response.status(), StatusCode::OK);

        let body = response_body_json(response).await;

        assert_eq!(body["limits"]["max_limit"], 500);
        assert!(body["datasets"].as_array().unwrap().iter().any(|dataset| {
            dataset["name"] == "requests"
                && dataset["default_fields"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("started_at"))
        }));
    }

    #[tokio::test]
    async fn structured_query_jsonl_response_serializes_rows_and_headers() {
        let response = structured_query_jsonl_response(json!({
            "dataset": "requests",
            "fields": ["id", "status"],
            "rows": [
                {"id": "trace-1", "status": 200},
                {"id": "trace-2", "status": 500}
            ],
            "limit": 100
        }));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            JSONL_CONTENT_TYPE
        );
        assert_eq!(
            response.headers().get(header::CONTENT_DISPOSITION).unwrap(),
            "attachment; filename=\"llmtrace-requests.jsonl\""
        );
        assert_eq!(response.headers().get(EXPORT_ROWS_HEADER).unwrap(), "2");

        let body = response_body_string(response).await;

        assert!(body.ends_with('\n'));
        let lines = body
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            lines,
            vec![
                json!({"id": "trace-1", "status": 200}),
                json!({"id": "trace-2", "status": 500})
            ]
        );
    }

    #[tokio::test]
    async fn structured_query_jsonl_response_allows_empty_exports() {
        let response = structured_query_jsonl_response(json!({
            "dataset": "sessions",
            "fields": ["id"],
            "rows": [],
            "limit": 100
        }));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get(EXPORT_ROWS_HEADER).unwrap(), "0");

        let body = response_body_string(response).await;

        assert!(body.is_empty());
    }

    #[test]
    fn request_search_normalization_trims_empty_values() {
        assert_eq!(normalize_request_search(None).unwrap(), None);
        assert_eq!(
            normalize_request_search(Some("   ".to_string())).unwrap(),
            None
        );
        assert_eq!(
            normalize_request_search(Some("  gpt-4o  ".to_string())).unwrap(),
            Some("gpt-4o".to_string())
        );
    }

    #[test]
    fn request_search_normalization_rejects_oversized_values() {
        let error =
            normalize_request_search(Some("a".repeat(MAX_REQUEST_SEARCH_BYTES + 1))).unwrap_err();

        assert!(error.contains("q must be at most"));
    }

    #[test]
    fn request_filter_normalization_trims_empty_values() {
        assert_eq!(normalize_request_filter("model", None).unwrap(), None);
        assert_eq!(
            normalize_request_filter("model", Some("   ".to_string())).unwrap(),
            None
        );
        assert_eq!(
            normalize_request_filter("model", Some(" gpt-4o-mini ".to_string())).unwrap(),
            Some("gpt-4o-mini".to_string())
        );
    }

    #[test]
    fn request_filter_normalization_rejects_oversized_values() {
        let error =
            normalize_request_filter("model", Some("a".repeat(MAX_REQUEST_FILTER_BYTES + 1)))
                .unwrap_err();

        assert!(error.contains("model must be at most"));
    }

    #[test]
    fn request_kind_filter_normalization_accepts_known_values() {
        assert_eq!(
            normalize_request_kind_filter(Some(" websocket ".to_string())).unwrap(),
            Some("websocket".to_string())
        );
    }

    #[test]
    fn request_kind_filter_normalization_rejects_unknown_values() {
        let error = normalize_request_kind_filter(Some("unknown".to_string())).unwrap_err();

        assert!(error.contains("request_kind must be one of"));
    }

    #[test]
    fn status_class_filter_normalization_accepts_known_values() {
        assert_eq!(
            normalize_status_class_filter(Some(" 5XX ".to_string())).unwrap(),
            Some("5xx".to_string())
        );
        assert_eq!(
            normalize_status_class_filter(Some(" no_status ".to_string())).unwrap(),
            Some("no_status".to_string())
        );
    }

    #[test]
    fn status_class_filter_normalization_rejects_unknown_values() {
        let error = normalize_status_class_filter(Some("success".to_string())).unwrap_err();

        assert!(error.contains("status_class must be one of"));
    }

    #[test]
    fn optional_uuid_filter_normalization_accepts_valid_uuid() {
        let id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();

        assert_eq!(
            normalize_optional_uuid_filter("session_id", Some(format!(" {id} "))).unwrap(),
            Some(id)
        );
    }

    #[test]
    fn optional_uuid_filter_normalization_rejects_invalid_uuid() {
        let error = normalize_optional_uuid_filter("session_id", Some("not-a-uuid".to_string()))
            .unwrap_err();

        assert!(error.contains("session_id must be a UUID"));
    }

    #[test]
    fn request_time_range_normalization_accepts_rfc3339_bounds() {
        let range = normalize_request_time_range(
            Some(" 2026-06-01T12:00:00Z ".to_string()),
            Some("2026-06-01T12:30:00+00:00".to_string()),
        )
        .unwrap();

        assert_eq!(
            range.since.unwrap(),
            DateTime::parse_from_rfc3339("2026-06-01T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
        );
        assert_eq!(
            range.until.unwrap(),
            DateTime::parse_from_rfc3339("2026-06-01T12:30:00Z")
                .unwrap()
                .with_timezone(&Utc)
        );
    }

    #[test]
    fn request_time_range_normalization_trims_empty_values() {
        assert_eq!(
            normalize_request_time_range(None, Some("   ".to_string())).unwrap(),
            RequestTimeRange {
                since: None,
                until: None
            }
        );
    }

    #[test]
    fn request_time_range_normalization_rejects_invalid_values() {
        let error = normalize_request_time_range(Some("not-a-time".to_string()), None).unwrap_err();

        assert!(error.contains("since must be an RFC3339 timestamp"));
    }

    #[test]
    fn request_time_range_normalization_rejects_oversized_values() {
        let error =
            normalize_request_time_range(Some("a".repeat(MAX_REQUEST_TIME_FILTER_BYTES + 1)), None)
                .unwrap_err();

        assert!(error.contains("since must be at most"));
    }

    #[test]
    fn request_time_range_normalization_rejects_reversed_bounds() {
        let error = normalize_request_time_range(
            Some("2026-06-01T12:30:00Z".to_string()),
            Some("2026-06-01T12:00:00Z".to_string()),
        )
        .unwrap_err();

        assert!(error.contains("since must be earlier than or equal to until"));
    }

    #[test]
    fn request_duration_range_normalization_accepts_bounds() {
        let range = normalize_request_duration_range(Some(250), Some(1_000)).unwrap();

        assert_eq!(
            range,
            RequestDurationRange {
                min_duration_ms: Some(250),
                max_duration_ms: Some(1_000),
            }
        );
    }

    #[test]
    fn request_duration_range_normalization_accepts_empty_bounds() {
        let range = normalize_request_duration_range(None, None).unwrap();

        assert_eq!(
            range,
            RequestDurationRange {
                min_duration_ms: None,
                max_duration_ms: None,
            }
        );
    }

    #[test]
    fn request_duration_range_normalization_rejects_negative_values() {
        let error = normalize_request_duration_range(Some(-1), None).unwrap_err();

        assert!(error.contains("min_duration_ms must be greater than or equal to 0"));
    }

    #[test]
    fn request_duration_range_normalization_rejects_reversed_bounds() {
        let error = normalize_request_duration_range(Some(1_000), Some(250)).unwrap_err();

        assert!(error.contains("min_duration_ms must be less than or equal to max_duration_ms"));
    }

    #[test]
    fn optional_filter_normalization_trims_empty_values() {
        assert_eq!(normalize_optional_filter("event_type", None).unwrap(), None);
        assert_eq!(
            normalize_optional_filter("event_type", Some("   ".to_string())).unwrap(),
            None
        );
        assert_eq!(
            normalize_optional_filter("event_type", Some(" login_failed ".to_string())).unwrap(),
            Some("login_failed".to_string())
        );
    }

    #[test]
    fn optional_filter_normalization_rejects_oversized_values() {
        let error =
            normalize_optional_filter("user_id", Some("a".repeat(MAX_AUDIT_FILTER_BYTES + 1)))
                .unwrap_err();

        assert!(error.contains("user_id must be at most"));
    }

    #[test]
    fn usage_timeseries_bucket_normalization_accepts_known_values() {
        assert_eq!(normalize_usage_timeseries_bucket(None).unwrap(), None);
        assert_eq!(
            normalize_usage_timeseries_bucket(Some("   ".to_string())).unwrap(),
            None
        );
        assert_eq!(
            normalize_usage_timeseries_bucket(Some(" Hour ".to_string())).unwrap(),
            Some("hour".to_string())
        );
        assert_eq!(
            normalize_usage_timeseries_bucket(Some("day".to_string())).unwrap(),
            Some("day".to_string())
        );
    }

    #[test]
    fn usage_timeseries_bucket_normalization_rejects_unknown_values() {
        let error = normalize_usage_timeseries_bucket(Some("week".to_string())).unwrap_err();

        assert!(error.contains("bucket must be one of"));
    }

    async fn response_body_json(response: Response) -> serde_json::Value {
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn response_body_string(response: Response) -> String {
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(body.to_vec()).unwrap()
    }
}
