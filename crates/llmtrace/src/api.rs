use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::auth::MeResponse;
use crate::config::Config;
use crate::plugins::PluginStatus;
use crate::redaction;
use crate::state::AppState;
use crate::storage;
use crate::types::PluginHook;

const MAX_REQUEST_SEARCH_BYTES: usize = 512;
const MAX_REQUEST_FILTER_BYTES: usize = 1024;
const MAX_REQUEST_TIME_FILTER_BYTES: usize = 128;
const MAX_AUDIT_FILTER_BYTES: usize = 1024;
const MAX_REDACTION_PREVIEW_HEADERS: usize = 64;
const MAX_REDACTION_PREVIEW_HEADER_NAME_BYTES: usize = 128;
const MAX_REDACTION_PREVIEW_HEADER_VALUE_BYTES: usize = 8 * 1024;
const MAX_REDACTION_PREVIEW_URI_BYTES: usize = 8 * 1024;
const MAX_REDACTION_PREVIEW_BODY_BYTES: usize = 64 * 1024;
const UI_SESSION_HASH_BYTES: usize = 64;
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
    api_key_hash: Option<String>,
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
struct RecentErrorRequestsQuery {
    since_hours: Option<i64>,
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct SlowRequestsQuery {
    since_hours: Option<i64>,
    min_duration_ms: Option<i64>,
    limit: Option<i64>,
    offset: Option<i64>,
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
struct UiSessionListQuery {
    include_expired: Option<bool>,
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
struct AuditSummaryQuery {
    since_hours: Option<i64>,
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct UsageSummaryQuery {
    since_hours: Option<i64>,
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct ApiKeyUsageQuery {
    since_hours: Option<i64>,
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct ModelUsageQuery {
    since_hours: Option<i64>,
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct UserUsageQuery {
    since_hours: Option<i64>,
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct UpstreamHealthQuery {
    since_hours: Option<i64>,
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct ErrorSummaryQuery {
    since_hours: Option<i64>,
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct LatencySummaryQuery {
    since_hours: Option<i64>,
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct UsageTimeseriesQuery {
    since_hours: Option<i64>,
    bucket: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DataOverviewQuery {
    since_hours: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct DataIntegrityQuery {
    since_hours: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct RedactionPreviewRequest {
    headers: Option<BTreeMap<String, String>>,
    uri: Option<String>,
    body: Option<String>,
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
        .route("/config", get(runtime_config))
        .route("/security/posture", get(security_posture_report))
        .route("/retention/status", get(retention_status))
        .route("/storage/summary", get(storage_summary))
        .route("/redaction/preview", post(redaction_preview))
        .route("/usage/summary", get(usage_summary))
        .route("/usage/api-keys", get(api_key_usage))
        .route("/usage/models", get(model_usage))
        .route("/usage/users", get(user_usage))
        .route("/usage/upstreams", get(upstream_health))
        .route("/usage/errors", get(error_summary))
        .route("/usage/latency", get(latency_summary))
        .route("/usage/timeseries", get(usage_timeseries))
        .route("/data/overview", get(data_overview))
        .route("/data/integrity", get(data_integrity))
        .route("/requests", get(list_requests))
        .route("/requests/facets", get(request_facets))
        .route("/requests/recent-errors", get(recent_error_requests))
        .route("/requests/slow", get(slow_requests))
        .route("/requests/export.jsonl", get(export_requests_jsonl))
        .route("/requests/{id}", get(get_request))
        .route("/sessions", get(list_sessions))
        .route(
            "/sessions/{id}/requests/export.jsonl",
            get(export_session_requests_jsonl),
        )
        .route("/sessions/{id}/requests", get(list_session_requests))
        .route(
            "/sessions/{id}/messages/export.jsonl",
            get(export_session_messages_jsonl),
        )
        .route("/sessions/{id}", get(get_session))
        .route("/ui-sessions", get(list_ui_sessions))
        .route("/ui-sessions/{session_hash}", delete(revoke_ui_session))
        .route("/audit-events/export.jsonl", get(export_audit_events_jsonl))
        .route("/audit-events/summary", get(audit_summary))
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

async fn runtime_config(State(state): State<AppState>) -> Response {
    Json(config_summary(state.config.as_ref())).into_response()
}

async fn security_posture_report(State(state): State<AppState>) -> Response {
    Json(security_posture(state.config.as_ref())).into_response()
}

async fn retention_status(State(state): State<AppState>) -> Response {
    match storage::retention_status(
        &state.pool,
        state.config.storage.retention_days,
        state.config.storage.retention_prune_interval_secs,
        state.config.storage.retention_prune_batch_size,
    )
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn storage_summary(State(state): State<AppState>) -> Response {
    match storage::storage_summary(&state.pool).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn redaction_preview(
    State(state): State<AppState>,
    Json(payload): Json<RedactionPreviewRequest>,
) -> Response {
    match build_redaction_preview(state.config.as_ref(), payload) {
        Ok(value) => Json(value).into_response(),
        Err(message) => bad_request(message),
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

async fn api_key_usage(
    State(state): State<AppState>,
    Query(query): Query<ApiKeyUsageQuery>,
) -> Response {
    match storage::api_key_usage(&state.pool, query.since_hours, query.limit).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn model_usage(
    State(state): State<AppState>,
    Query(query): Query<ModelUsageQuery>,
) -> Response {
    match storage::model_usage(&state.pool, query.since_hours, query.limit).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn user_usage(
    State(state): State<AppState>,
    Query(query): Query<UserUsageQuery>,
) -> Response {
    match storage::user_usage(&state.pool, query.since_hours, query.limit).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn upstream_health(
    State(state): State<AppState>,
    Query(query): Query<UpstreamHealthQuery>,
) -> Response {
    match storage::upstream_health(&state.pool, query.since_hours, query.limit).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn error_summary(
    State(state): State<AppState>,
    Query(query): Query<ErrorSummaryQuery>,
) -> Response {
    match storage::error_summary(&state.pool, query.since_hours, query.limit).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn latency_summary(
    State(state): State<AppState>,
    Query(query): Query<LatencySummaryQuery>,
) -> Response {
    match storage::latency_summary(&state.pool, query.since_hours, query.limit).await {
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

async fn data_overview(
    State(state): State<AppState>,
    Query(query): Query<DataOverviewQuery>,
) -> Response {
    match storage::data_overview(&state.pool, query.since_hours).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn data_integrity(
    State(state): State<AppState>,
    Query(query): Query<DataIntegrityQuery>,
) -> Response {
    match storage::data_integrity(&state.pool, query.since_hours).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn list_requests(
    State(state): State<AppState>,
    Query(query): Query<RequestListQuery>,
) -> Response {
    let filters = match request_list_filters(query) {
        Ok(filters) => filters,
        Err(message) => return bad_request(message),
    };

    match storage::list_requests(&state.pool, filters).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn export_requests_jsonl(
    State(state): State<AppState>,
    Query(query): Query<RequestListQuery>,
) -> Response {
    let filters = match request_list_filters(query) {
        Ok(filters) => filters,
        Err(message) => return bad_request(message),
    };

    match storage::list_requests(&state.pool, filters).await {
        Ok(value) => request_list_jsonl_response(value),
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

async fn recent_error_requests(
    State(state): State<AppState>,
    Query(query): Query<RecentErrorRequestsQuery>,
) -> Response {
    match storage::recent_error_requests(&state.pool, query.since_hours, query.limit).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn slow_requests(
    State(state): State<AppState>,
    Query(query): Query<SlowRequestsQuery>,
) -> Response {
    let min_duration_ms =
        match normalize_optional_duration_filter("min_duration_ms", query.min_duration_ms) {
            Ok(value) => value,
            Err(message) => return bad_request(message),
        };

    match storage::slow_requests(
        &state.pool,
        query.since_hours,
        min_duration_ms,
        query.limit,
        query.offset,
    )
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn get_request(State(state): State<AppState>, Path(id): Path<Uuid>) -> Response {
    match storage::get_request(&state.pool, id, &state.config.archive).await {
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

async fn export_session_requests_jsonl(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<SessionRequestListQuery>,
) -> Response {
    match storage::list_session_requests(&state.pool, id, query.limit, query.offset).await {
        Ok(Some(value)) => request_list_jsonl_response(value),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "session not found"})),
        )
            .into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn export_session_messages_jsonl(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<SessionDetailQuery>,
) -> Response {
    match storage::get_session(&state.pool, id, query.messages_limit, query.messages_offset).await {
        Ok(Some(value)) => session_messages_jsonl_response(value),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "session not found"})),
        )
            .into_response(),
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

async fn list_ui_sessions(
    State(state): State<AppState>,
    Query(query): Query<UiSessionListQuery>,
) -> Response {
    match storage::list_ui_sessions(
        &state.pool,
        query.include_expired.unwrap_or(false),
        query.limit,
        query.offset,
    )
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn revoke_ui_session(
    State(state): State<AppState>,
    Extension(user): Extension<MeResponse>,
    Path(session_hash): Path<String>,
) -> Response {
    let session_hash = match normalize_ui_session_hash(session_hash) {
        Ok(session_hash) => session_hash,
        Err(message) => return bad_request(message),
    };

    match storage::revoke_ui_session(&state.pool, &session_hash).await {
        Ok(Some(session)) => {
            let audit_result = storage::record_ui_audit_event(
                &state.pool,
                "ui_session_revoked",
                Some(&user.user_id),
                json!({"session_hash": session_hash}),
            )
            .await;
            if let Err(error) = audit_result {
                tracing::warn!(%error, "failed to record ui session revocation audit event");
            }

            Json(json!({
                "revoked": true,
                "session": session,
            }))
            .into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "ui session not found"})),
        )
            .into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn list_audit_events(
    State(state): State<AppState>,
    Query(query): Query<AuditEventListQuery>,
) -> Response {
    let (event_type, user_id) = match audit_event_filters(query.event_type, query.user_id) {
        Ok(filters) => filters,
        Err(message) => return bad_request(message),
    };

    match storage::list_audit_events(&state.pool, event_type, user_id, query.limit, query.offset)
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn export_audit_events_jsonl(
    State(state): State<AppState>,
    Query(query): Query<AuditEventListQuery>,
) -> Response {
    let (event_type, user_id) = match audit_event_filters(query.event_type, query.user_id) {
        Ok(filters) => filters,
        Err(message) => return bad_request(message),
    };

    match storage::list_audit_events(&state.pool, event_type, user_id, query.limit, query.offset)
        .await
    {
        Ok(value) => audit_event_jsonl_response(value),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn audit_summary(
    State(state): State<AppState>,
    Query(query): Query<AuditSummaryQuery>,
) -> Response {
    match storage::audit_summary(&state.pool, query.since_hours, query.limit).await {
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
    Json(plugin_status_summary(state.plugins.statuses())).into_response()
}

fn build_redaction_preview(
    config: &Config,
    payload: RedactionPreviewRequest,
) -> Result<Value, String> {
    Ok(json!({
        "redaction": {
            "store_header_hash": config.redaction.store_header_hash,
            "sensitive_header_count": config.redaction.sensitive_headers.len(),
            "upstream_header": &config.proxy.upstream_header,
            "limits": {
                "max_headers": MAX_REDACTION_PREVIEW_HEADERS,
                "max_header_name_bytes": MAX_REDACTION_PREVIEW_HEADER_NAME_BYTES,
                "max_header_value_bytes": MAX_REDACTION_PREVIEW_HEADER_VALUE_BYTES,
                "max_uri_bytes": MAX_REDACTION_PREVIEW_URI_BYTES,
                "max_body_bytes": MAX_REDACTION_PREVIEW_BODY_BYTES,
            },
        },
        "headers": redaction_preview_headers(config, payload.headers)?,
        "uri": redaction_preview_uri(payload.uri)?,
        "body": redaction_preview_body(payload.body)?,
    }))
}

fn redaction_preview_headers(
    config: &Config,
    headers: Option<BTreeMap<String, String>>,
) -> Result<Value, String> {
    let Some(headers) = headers else {
        return Ok(json!({
            "provided": false,
            "redacted": {},
            "first_secret_header_hash": Value::Null,
        }));
    };
    if headers.len() > MAX_REDACTION_PREVIEW_HEADERS {
        return Err(format!(
            "headers must contain at most {MAX_REDACTION_PREVIEW_HEADERS} entries"
        ));
    }

    let mut header_map = HeaderMap::new();
    for (name, value) in headers {
        let name = name.trim();
        if name.is_empty() {
            return Err("header names must not be empty".to_string());
        }
        if name.len() > MAX_REDACTION_PREVIEW_HEADER_NAME_BYTES {
            return Err(format!(
                "header names must be at most {MAX_REDACTION_PREVIEW_HEADER_NAME_BYTES} bytes"
            ));
        }
        if value.len() > MAX_REDACTION_PREVIEW_HEADER_VALUE_BYTES {
            return Err(format!(
                "header {name:?} value must be at most {MAX_REDACTION_PREVIEW_HEADER_VALUE_BYTES} bytes"
            ));
        }

        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| format!("header {name:?} has an invalid name"))?;
        let header_value = HeaderValue::from_str(&value)
            .map_err(|_| format!("header {name:?} has an invalid value"))?;
        header_map.insert(header_name, header_value);
    }

    let redacted = redaction::redact_headers(
        &header_map,
        &config.redaction,
        &config.proxy.upstream_header,
    );
    Ok(json!({
        "provided": true,
        "redacted": redacted.json,
        "first_secret_header_hash": redacted.first_secret_hash,
    }))
}

fn redaction_preview_uri(uri: Option<String>) -> Result<Value, String> {
    let Some(uri) = uri else {
        return Ok(json!({
            "provided": false,
            "redacted": Value::Null,
        }));
    };
    let uri = uri.trim();
    if uri.is_empty() {
        return Ok(json!({
            "provided": false,
            "redacted": Value::Null,
        }));
    }
    if uri.len() > MAX_REDACTION_PREVIEW_URI_BYTES {
        return Err(format!(
            "uri must be at most {MAX_REDACTION_PREVIEW_URI_BYTES} bytes"
        ));
    }

    let redacted = redaction::redact_uri_query_values(uri);
    Ok(json!({
        "provided": true,
        "input_bytes": uri.len(),
        "output_bytes": redacted.len(),
        "changed": redacted != uri,
        "redacted": redacted,
    }))
}

fn redaction_preview_body(body: Option<String>) -> Result<Value, String> {
    let Some(body) = body else {
        return Ok(json!({
            "provided": false,
            "redacted": Value::Null,
        }));
    };
    if body.len() > MAX_REDACTION_PREVIEW_BODY_BYTES {
        return Err(format!(
            "body must be at most {MAX_REDACTION_PREVIEW_BODY_BYTES} bytes"
        ));
    }

    Ok(json!({
        "provided": true,
        "input_bytes": body.len(),
        "output_bytes": body.len(),
        "changed": false,
        "dropped": false,
        "redacted": body,
    }))
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

    jsonl_response(
        rows,
        &format!("llmtrace-{dataset}.jsonl"),
        "llmtrace-query.jsonl",
    )
}

fn request_list_jsonl_response(value: Value) -> Response {
    let Some(rows) = value.get("items").and_then(Value::as_array) else {
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            anyhow::anyhow!("request list result did not contain an item array"),
        );
    };

    jsonl_response(rows, "llmtrace-requests.jsonl", "llmtrace-requests.jsonl")
}

fn session_messages_jsonl_response(value: Value) -> Response {
    let Some(rows) = value.get("messages").and_then(Value::as_array) else {
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            anyhow::anyhow!("session result did not contain a message array"),
        );
    };

    jsonl_response(
        rows,
        "llmtrace-session-messages.jsonl",
        "llmtrace-session-messages.jsonl",
    )
}

fn audit_event_jsonl_response(value: Value) -> Response {
    let Some(rows) = value.get("items").and_then(Value::as_array) else {
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            anyhow::anyhow!("audit event list result did not contain an item array"),
        );
    };

    jsonl_response(
        rows,
        "llmtrace-audit-events.jsonl",
        "llmtrace-audit-events.jsonl",
    )
}

fn jsonl_response(rows: &[Value], filename: &str, fallback_filename: &'static str) -> Response {
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
    let disposition = format!("attachment; filename=\"{filename}\"");
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition).unwrap_or_else(|_| {
            HeaderValue::from_str(&format!("attachment; filename=\"{fallback_filename}\""))
                .unwrap_or_else(|_| HeaderValue::from_static("attachment"))
        }),
    );
    headers.insert(
        EXPORT_ROWS_HEADER,
        HeaderValue::from_str(&rows.len().to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
    response
}

fn request_list_filters(query: RequestListQuery) -> Result<storage::RequestListFilters, String> {
    let q = normalize_request_search(query.q)?;
    let time_range = normalize_request_time_range(query.since, query.until)?;
    let duration_range =
        normalize_request_duration_range(query.min_duration_ms, query.max_duration_ms)?;
    let upstream_host = normalize_request_filter("upstream_host", query.upstream_host)?;
    let model = normalize_request_filter("model", query.model)?;
    let request_kind = normalize_request_kind_filter(query.request_kind)?;
    let session_id = normalize_optional_uuid_filter("session_id", query.session_id)?;
    let api_key_hash = normalize_request_filter("api_key_hash", query.api_key_hash)?;
    let status_class = normalize_status_class_filter(query.status_class)?;

    Ok(storage::RequestListFilters {
        q,
        status: query.status,
        status_class,
        has_error: query.has_error,
        upstream_host,
        model,
        request_kind,
        session_id,
        api_key_hash,
        since: time_range.since,
        until: time_range.until,
        min_duration_ms: duration_range.min_duration_ms,
        max_duration_ms: duration_range.max_duration_ms,
        limit: query.limit,
        offset: query.offset,
    })
}

fn audit_event_filters(
    event_type: Option<String>,
    user_id: Option<String>,
) -> Result<(Option<String>, Option<String>), String> {
    Ok((
        normalize_optional_filter("event_type", event_type)?,
        normalize_optional_filter("user_id", user_id)?,
    ))
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

fn normalize_ui_session_hash(value: String) -> Result<String, String> {
    let value = value.trim();
    if value.len() != UI_SESSION_HASH_BYTES {
        return Err(format!(
            "session_hash must be a {UI_SESSION_HASH_BYTES}-character hexadecimal SHA-256 value"
        ));
    }
    if !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("session_hash must contain only hexadecimal characters".to_string());
    }
    Ok(value.to_ascii_lowercase())
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

fn config_summary(config: &Config) -> Value {
    json!({
        "server": {
            "listen": &config.server.listen,
            "public_url": &config.server.public_url,
            "deployment": config.server.deployment.as_str(),
            "ui_enabled": config.server.ui_enabled,
        },
        "proxy": {
            "default_upstream": &config.proxy.default_upstream,
            "allow_upstreams": &config.proxy.allow_upstreams,
            "allow_upstreams_count": config.proxy.allow_upstreams.len(),
            "upstream_header": &config.proxy.upstream_header,
            "timeout_secs": config.proxy.timeout_secs,
            "max_request_body_bytes": config.proxy.max_request_body_bytes,
            "max_response_body_bytes": config.proxy.max_response_body_bytes,
            "max_websocket_message_bytes": config.proxy.max_websocket_message_bytes,
            "max_websocket_session_bytes": config.proxy.max_websocket_session_bytes,
        },
        "archive": {
            "storage_backend": config.archive.storage_backend.as_str(),
            "filesystem_root": &config.archive.filesystem_root,
            "segment_uncompressed_bytes": config.archive.segment_uncompressed_bytes,
            "compression_level": config.archive.compression_level,
        },
        "storage": {
            "max_connections": config.storage.max_connections,
            "acquire_timeout_secs": config.storage.acquire_timeout_secs,
            "trace_queue_capacity": config.storage.trace_queue_capacity,
            "trace_worker_count": config.storage.trace_worker_count,
            "retention_days": config.storage.retention_days,
            "retention_prune_interval_secs": config.storage.retention_prune_interval_secs,
            "retention_prune_batch_size": config.storage.retention_prune_batch_size,
        },
        "auth": {
            "cookie_secure": config.auth.cookie_secure,
            "session_ttl_hours": config.auth.session_ttl_hours,
            "local_admin": {
                "enabled": local_admin_configured(config),
                "password_hash_configured": config.auth.local_admin.password_hash.as_deref().is_some_and(|value| !value.trim().is_empty()),
                "plaintext_password_configured": config.auth.local_admin.password.as_deref().is_some_and(|value| !value.trim().is_empty()),
            },
            "login_rate_limit": {
                "enabled": config.auth.login_rate_limit.enabled,
                "max_failures": config.auth.login_rate_limit.max_failures,
                "window_secs": config.auth.login_rate_limit.window_secs,
                "lockout_secs": config.auth.login_rate_limit.lockout_secs,
                "max_tracked_entries": config.auth.login_rate_limit.max_tracked_entries,
            },
            "oauth": {
                "enabled": config.auth.oauth.enabled,
                "issuer_url": &config.auth.oauth.issuer_url,
                "redirect_url": &config.auth.oauth.redirect_url,
                "client_id_configured": !config.auth.oauth.client_id.trim().is_empty(),
                "client_secret_configured": !config.auth.oauth.client_secret.trim().is_empty(),
                "timeout_secs": config.auth.oauth.timeout_secs,
                "require_email_verified": config.auth.oauth.require_email_verified,
                "allowed_email_count": config.auth.oauth.allowed_emails.len(),
                "allowed_domain_count": config.auth.oauth.allowed_domains.len(),
            },
        },
        "observability": {
            "metrics_bearer_token_configured": config.observability.metrics_bearer_token.as_deref().is_some_and(|value| !value.trim().is_empty()),
        },
        "redaction": {
            "sensitive_header_count": config.redaction.sensitive_headers.len(),
            "store_header_hash": config.redaction.store_header_hash,
            "body_storage": "unredacted_archive",
        },
        "plugins": {
            "configured_count": config.plugins.len(),
        },
    })
}

fn security_posture(config: &Config) -> Value {
    let production = config.server.deployment.as_str() == "production";
    let public_url_https = url::Url::parse(&config.server.public_url)
        .is_ok_and(|url| url.scheme().eq_ignore_ascii_case("https"));
    let local_plaintext_password = config
        .auth
        .local_admin
        .password
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    let local_password_hash = config
        .auth
        .local_admin
        .password_hash
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    let metrics_token = config
        .observability
        .metrics_bearer_token
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    let oauth_allowlist = !config.auth.oauth.allowed_emails.is_empty()
        || !config.auth.oauth.allowed_domains.is_empty();

    let mut checks = Vec::new();
    push_posture_check(
        &mut checks,
        "deployment_mode",
        production,
        "warn",
        "server.deployment is production",
        "server.deployment is development; set production before public deployment",
    );
    push_posture_check(
        &mut checks,
        "public_url_https",
        public_url_https,
        "warn",
        "server.public_url uses HTTPS",
        "server.public_url is not HTTPS; this can be acceptable behind an HTTP-only reverse proxy, but browser traffic should terminate at HTTPS",
    );
    push_posture_check(
        &mut checks,
        "secure_cookies",
        config.auth.cookie_secure,
        "warn",
        "session cookies are marked Secure",
        "session cookies are not marked Secure; enable auth.cookie_secure when browser traffic uses HTTPS",
    );
    push_posture_check(
        &mut checks,
        "retention",
        config.storage.retention_days.is_some(),
        "warn",
        "storage retention is configured",
        "storage.retention_days is not configured; captured data will grow until manually pruned",
    );
    push_posture_check(
        &mut checks,
        "upstream_allowlist",
        !config.proxy.allow_upstreams.is_empty(),
        "warn",
        "proxy upstream allowlist is configured",
        "proxy.allow_upstreams is empty; restrict upstream overrides before public deployment",
    );
    push_posture_check(
        &mut checks,
        "metrics_auth",
        metrics_token,
        "warn",
        "metrics endpoint requires a bearer token",
        "metrics bearer token is not configured; protect /metrics before public deployment",
    );
    push_posture_check_with_status(
        &mut checks,
        "local_plaintext_password",
        if local_plaintext_password {
            if production { "fail" } else { "warn" }
        } else {
            "pass"
        },
        "local admin plaintext password is not configured",
        "local admin plaintext password is configured; use auth.local_admin.password_hash before production",
    );
    push_posture_check(
        &mut checks,
        "local_password_hash",
        local_password_hash || config.auth.oauth.enabled,
        "warn",
        "local password hash or OAuth authentication is configured",
        "no local password hash or OAuth provider is configured for production authentication",
    );
    push_posture_check_with_status(
        &mut checks,
        "login_rate_limit",
        if config.auth.login_rate_limit.enabled {
            "pass"
        } else if production {
            "fail"
        } else {
            "warn"
        },
        "login rate limiting is enabled",
        "login rate limiting is disabled; enable it before public deployment",
    );
    push_posture_check_with_status(
        &mut checks,
        "oauth_allowlist",
        if !config.auth.oauth.enabled || oauth_allowlist {
            "pass"
        } else if production {
            "fail"
        } else {
            "warn"
        },
        "OAuth is disabled or constrained by an allowlist",
        "OAuth is enabled without allowed_emails or allowed_domains; constrain who can sign in",
    );

    let pass_count = posture_status_count(&checks, "pass");
    let warn_count = posture_status_count(&checks, "warn");
    let fail_count = posture_status_count(&checks, "fail");
    let overall = if fail_count > 0 {
        "fail"
    } else if warn_count > 0 {
        "attention"
    } else {
        "ready"
    };

    json!({
        "overall": overall,
        "counts": {
            "pass": pass_count,
            "warn": warn_count,
            "fail": fail_count,
        },
        "checks": checks,
    })
}

fn push_posture_check(
    checks: &mut Vec<Value>,
    id: &'static str,
    passed: bool,
    failing_status: &'static str,
    pass_message: &'static str,
    failing_message: &'static str,
) {
    push_posture_check_with_status(
        checks,
        id,
        if passed { "pass" } else { failing_status },
        pass_message,
        failing_message,
    );
}

fn push_posture_check_with_status(
    checks: &mut Vec<Value>,
    id: &'static str,
    status: &'static str,
    pass_message: &'static str,
    failing_message: &'static str,
) {
    let message = if status == "pass" {
        pass_message
    } else {
        failing_message
    };
    checks.push(json!({
        "id": id,
        "status": status,
        "message": message,
    }));
}

fn posture_status_count(checks: &[Value], status: &str) -> usize {
    checks
        .iter()
        .filter(|check| check.get("status").and_then(Value::as_str) == Some(status))
        .count()
}

fn local_admin_configured(config: &Config) -> bool {
    config
        .auth
        .local_admin
        .password
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || config
            .auth
            .local_admin
            .password_hash
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
}

fn plugin_status_summary(statuses: &[PluginStatus]) -> Value {
    let loaded_count = statuses.iter().filter(|status| status.loaded).count();
    json!({
        "configured_count": statuses.len(),
        "loaded_count": loaded_count,
        "failed_count": statuses.len().saturating_sub(loaded_count),
        "items": statuses.iter().map(plugin_status_item).collect::<Vec<_>>(),
    })
}

fn plugin_status_item(status: &PluginStatus) -> Value {
    json!({
        "name": status.name,
        "hooks": status.hooks.iter().copied().map(plugin_hook_label).collect::<Vec<_>>(),
        "loaded": status.loaded,
        "error": status.error.as_ref().map(|_| "plugin failed to load"),
    })
}

fn plugin_hook_label(value: PluginHook) -> &'static str {
    value.as_str()
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;

    use crate::config::{DeploymentMode, PluginConfig};

    use super::*;

    const VALID_ARGON2_HASH: &str =
        "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHQ$k9wPtUZeX9pTvvFeUq1eYn5X2IN3QmEF7L7w8zZ3xIQ";

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

    #[test]
    fn config_summary_reports_runtime_settings_without_secret_values() {
        let config = Config {
            server: crate::config::ServerConfig {
                listen: "0.0.0.0:3000".to_string(),
                public_url: "https://llmtrace.example.com".to_string(),
                deployment: DeploymentMode::Production,
                ui_enabled: true,
            },
            proxy: crate::config::ProxyConfig {
                default_upstream: "https://api.openai.com".to_string(),
                allow_upstreams: vec!["api.openai.com".to_string()],
                upstream_header: "x-llmtrace-upstream".to_string(),
                timeout_secs: 120,
                max_request_body_bytes: 8192,
                max_response_body_bytes: 4096,
                max_websocket_message_bytes: 16384,
                max_websocket_session_bytes: 32768,
            },
            storage: crate::config::StorageConfig {
                postgres_url: "postgres://db-user:db-pass@db.internal/llmtrace".to_string(),
                retention_days: Some(30),
                ..Default::default()
            },
            archive: Default::default(),
            auth: crate::config::AuthConfig {
                cookie_secure: true,
                local_admin: crate::config::LocalAdminConfig {
                    username: "admin-user".to_string(),
                    password: Some("plain-secret".to_string()),
                    password_hash: Some("hash-secret".to_string()),
                },
                oauth: crate::config::OAuthConfig {
                    enabled: true,
                    issuer_url: "https://issuer.example.com".to_string(),
                    client_id: "client-id-secret-ish".to_string(),
                    client_secret: "oauth-client-secret".to_string(),
                    redirect_url: "https://llmtrace.example.com/api/auth/oauth/callback"
                        .to_string(),
                    allowed_emails: vec!["private-admin@secret-mail.test".to_string()],
                    allowed_domains: vec!["sensitive-tenant.test".to_string()],
                    ..Default::default()
                },
                ..Default::default()
            },
            observability: crate::config::ObservabilityConfig {
                metrics_bearer_token: Some("metrics-secret".to_string()),
            },
            redaction: Default::default(),
            plugins: vec![PluginConfig {
                name: "classifier".to_string(),
                wasm_path: "/secret/plugin/classifier.wasm".into(),
                hooks: vec![PluginHook::ResponseEnd],
                timeout_ms: 100,
            }],
        };

        let summary = config_summary(&config);

        assert_eq!(summary["server"]["deployment"], "production");
        assert_eq!(summary["proxy"]["allow_upstreams_count"], 1);
        assert_eq!(summary["storage"]["retention_days"], 30);
        assert_eq!(
            summary["auth"]["local_admin"]["password_hash_configured"],
            true
        );
        assert_eq!(
            summary["auth"]["local_admin"]["plaintext_password_configured"],
            true
        );
        assert_eq!(summary["auth"]["oauth"]["client_id_configured"], true);
        assert_eq!(summary["auth"]["oauth"]["client_secret_configured"], true);
        assert_eq!(summary["auth"]["oauth"]["allowed_email_count"], 1);
        assert_eq!(summary["auth"]["oauth"]["allowed_domain_count"], 1);
        assert_eq!(
            summary["observability"]["metrics_bearer_token_configured"],
            true
        );
        assert_eq!(summary["archive"]["storage_backend"], "filesystem");
        assert_eq!(summary["redaction"]["body_storage"], "unredacted_archive");
        assert_eq!(summary["plugins"]["configured_count"], 1);

        let serialized = serde_json::to_string(&summary).unwrap();
        for secret in [
            "postgres://",
            "db-pass",
            "admin-user",
            "plain-secret",
            "hash-secret",
            "client-id-secret-ish",
            "oauth-client-secret",
            "private-admin@secret-mail.test",
            "sensitive-tenant.test",
            "metrics-secret",
            "/secret/plugin/classifier.wasm",
        ] {
            assert!(
                !serialized.contains(secret),
                "{secret} leaked in {serialized}"
            );
        }
    }

    #[test]
    fn security_posture_reports_ready_for_hardened_config_without_secret_values() {
        let config = Config {
            server: crate::config::ServerConfig {
                listen: "0.0.0.0:3000".to_string(),
                public_url: "https://llmtrace.example.com".to_string(),
                deployment: DeploymentMode::Production,
                ui_enabled: true,
            },
            proxy: crate::config::ProxyConfig {
                allow_upstreams: vec!["api.openai.com".to_string()],
                ..Default::default()
            },
            storage: crate::config::StorageConfig {
                postgres_url: "postgres://db-user:db-pass@db.internal/llmtrace".to_string(),
                retention_days: Some(30),
                ..Default::default()
            },
            auth: crate::config::AuthConfig {
                cookie_secure: true,
                local_admin: crate::config::LocalAdminConfig {
                    username: "admin-user".to_string(),
                    password: None,
                    password_hash: Some(VALID_ARGON2_HASH.to_string()),
                },
                ..Default::default()
            },
            observability: crate::config::ObservabilityConfig {
                metrics_bearer_token: Some("metrics-secret".to_string()),
            },
            ..Default::default()
        };

        let posture = security_posture(&config);

        assert_eq!(posture["overall"], "ready");
        assert_eq!(posture["counts"]["fail"], 0);
        assert_eq!(posture["counts"]["warn"], 0);
        assert!(
            posture["checks"]
                .as_array()
                .unwrap()
                .iter()
                .all(|check| check["status"] == "pass")
        );

        let serialized = serde_json::to_string(&posture).unwrap();
        for secret in [
            "postgres://",
            "db-pass",
            "admin-user",
            VALID_ARGON2_HASH,
            "metrics-secret",
        ] {
            assert!(
                !serialized.contains(secret),
                "{secret} leaked in {serialized}"
            );
        }
    }

    #[test]
    fn security_posture_flags_development_defaults_without_failing_http_public_url() {
        let posture = security_posture(&Config::default());

        assert_eq!(posture["overall"], "attention");
        assert_eq!(posture["counts"]["fail"], 0);
        assert!(
            posture["counts"]["warn"]
                .as_u64()
                .is_some_and(|count| count > 0)
        );
        assert_eq!(
            posture_check_status(&posture, "deployment_mode"),
            Some("warn")
        );
        assert_eq!(
            posture_check_status(&posture, "public_url_https"),
            Some("warn")
        );
        assert_eq!(
            posture_check_status(&posture, "local_plaintext_password"),
            Some("warn")
        );
    }

    #[test]
    fn plugin_status_summary_omits_paths_and_raw_errors() {
        let statuses = vec![
            PluginStatus {
                name: "loaded-plugin".to_string(),
                wasm_path: "/srv/secret/plugins/loaded.wasm".into(),
                hooks: vec![PluginHook::RequestStart, PluginHook::ResponseEnd],
                loaded: true,
                error: None,
            },
            PluginStatus {
                name: "failed-plugin".to_string(),
                wasm_path: "/srv/secret/plugins/failed.wasm".into(),
                hooks: vec![PluginHook::ResponseHeaders],
                loaded: false,
                error: Some("failed to read /srv/secret/plugins/failed.wasm".to_string()),
            },
        ];

        let summary = plugin_status_summary(&statuses);

        assert_eq!(summary["configured_count"], 2);
        assert_eq!(summary["loaded_count"], 1);
        assert_eq!(summary["failed_count"], 1);
        assert_eq!(summary["items"][0]["hooks"][0], "on_request_start");
        assert_eq!(summary["items"][0]["hooks"][1], "on_response_end");
        assert_eq!(summary["items"][0]["error"], Value::Null);
        assert_eq!(summary["items"][1]["error"], "plugin failed to load");

        let serialized = serde_json::to_string(&summary).unwrap();
        assert!(!serialized.contains("/srv/secret/plugins"));
        assert!(!serialized.contains("failed to read"));
        assert!(!serialized.contains("failed.wasm"));
        assert!(!serialized.contains("loaded.wasm"));
    }

    #[test]
    fn redaction_preview_applies_header_and_uri_redaction_and_leaves_body_unchanged() {
        let mut config = Config::default();
        config.redaction.store_header_hash = true;

        let mut headers = std::collections::BTreeMap::new();
        headers.insert(
            "authorization".to_string(),
            "Bearer sk-redaction-preview".to_string(),
        );
        headers.insert("content-type".to_string(), "application/json".to_string());
        headers.insert(
            config.proxy.upstream_header.clone(),
            "https://proxy-user:proxy-pass@api.example.com/v1?api_key=upstream-secret".to_string(),
        );

        let preview = build_redaction_preview(
            &config,
            RedactionPreviewRequest {
                headers: Some(headers),
                uri: Some(
                    "https://url-user:url-pass@api.example.com/v1?api_key=url-secret&n=1"
                        .to_string(),
                ),
                body: Some(
                    r#"{"api_key":"sk-body-secret","messages":[{"content":"hello"}]}"#.to_string(),
                ),
            },
        )
        .unwrap();

        assert_eq!(preview["headers"]["provided"], true);
        assert_eq!(
            preview["headers"]["redacted"]["authorization"]["redacted"],
            true
        );
        assert!(
            preview["headers"]["first_secret_header_hash"]
                .as_str()
                .is_some_and(|hash| hash.len() == 64)
        );
        assert_eq!(
            preview["headers"]["redacted"]["content-type"],
            "application/json"
        );
        assert_eq!(
            preview["headers"]["redacted"]["x-llmtrace-upstream"]["redacted"],
            true
        );
        assert_eq!(
            preview["headers"]["redacted"]["x-llmtrace-upstream"]["url"],
            "https://api.example.com/v1?api_key=REDACTED"
        );
        assert_eq!(
            preview["uri"]["redacted"],
            "https://api.example.com/v1?api_key=REDACTED&n=REDACTED"
        );

        let preview_body: Value =
            serde_json::from_str(preview["body"]["redacted"].as_str().unwrap()).unwrap();
        assert_eq!(preview_body["api_key"], "sk-body-secret");
        assert_eq!(preview_body["messages"][0]["content"], "hello");
        assert_eq!(preview["body"]["changed"], false);

        let serialized = serde_json::to_string(&preview).unwrap();
        for secret in [
            "sk-redaction-preview",
            "proxy-user:proxy-pass",
            "upstream-secret",
            "url-user:url-pass",
            "url-secret",
        ] {
            assert!(
                !serialized.contains(secret),
                "{secret} leaked in {serialized}"
            );
        }
    }

    #[test]
    fn redaction_preview_body_reports_unmodified_payload() {
        let config = Config::default();

        let preview = build_redaction_preview(
            &config,
            RedactionPreviewRequest {
                headers: None,
                uri: None,
                body: Some("secret body".to_string()),
            },
        )
        .unwrap();

        assert_eq!(preview["body"]["provided"], true);
        assert_eq!(preview["body"]["changed"], false);
        assert_eq!(preview["body"]["dropped"], false);
        assert_eq!(preview["body"]["output_bytes"], "secret body".len());
        assert_eq!(preview["body"]["redacted"], "secret body");
    }

    #[test]
    fn redaction_preview_rejects_invalid_and_oversized_inputs() {
        let config = Config::default();

        let error = build_redaction_preview(
            &config,
            RedactionPreviewRequest {
                headers: None,
                uri: None,
                body: Some("x".repeat(MAX_REDACTION_PREVIEW_BODY_BYTES + 1)),
            },
        )
        .unwrap_err();
        assert!(error.contains("body must be at most"));

        let mut headers = std::collections::BTreeMap::new();
        headers.insert("bad header".to_string(), "value".to_string());
        let error = build_redaction_preview(
            &config,
            RedactionPreviewRequest {
                headers: Some(headers),
                uri: None,
                body: None,
            },
        )
        .unwrap_err();
        assert!(error.contains("invalid name"));

        let mut headers = std::collections::BTreeMap::new();
        for index in 0..=MAX_REDACTION_PREVIEW_HEADERS {
            headers.insert(format!("x-test-{index}"), "value".to_string());
        }
        let error = build_redaction_preview(
            &config,
            RedactionPreviewRequest {
                headers: Some(headers),
                uri: None,
                body: None,
            },
        )
        .unwrap_err();
        assert!(error.contains("headers must contain at most"));
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

    #[tokio::test]
    async fn request_list_jsonl_response_serializes_items_and_headers() {
        let response = request_list_jsonl_response(json!({
            "items": [
                {"id": "trace-1", "status": 200},
                {"id": "trace-2", "status": 500}
            ],
            "page": {"limit": 100, "offset": 0, "has_more": false, "next_offset": null}
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
    async fn request_list_jsonl_response_allows_empty_exports() {
        let response = request_list_jsonl_response(json!({
            "items": [],
            "page": {"limit": 100, "offset": 0, "has_more": false, "next_offset": null}
        }));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get(EXPORT_ROWS_HEADER).unwrap(), "0");

        let body = response_body_string(response).await;

        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn session_messages_jsonl_response_serializes_messages_and_headers() {
        let response = session_messages_jsonl_response(json!({
            "id": "e1f806fd-4dd8-4b44-b398-3bbd58f7c821",
            "messages": [
                {"id": 1, "role": "user", "content": "hello"},
                {"id": 2, "role": "assistant", "content": "hi"}
            ],
            "messages_page": {"limit": 100, "offset": 0, "has_more": false, "next_offset": null}
        }));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            JSONL_CONTENT_TYPE
        );
        assert_eq!(
            response.headers().get(header::CONTENT_DISPOSITION).unwrap(),
            "attachment; filename=\"llmtrace-session-messages.jsonl\""
        );
        assert_eq!(response.headers().get(EXPORT_ROWS_HEADER).unwrap(), "2");

        let body = response_body_string(response).await;
        let lines = body
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(
            lines,
            vec![
                json!({"id": 1, "role": "user", "content": "hello"}),
                json!({"id": 2, "role": "assistant", "content": "hi"})
            ]
        );
    }

    #[tokio::test]
    async fn session_messages_jsonl_response_allows_empty_exports() {
        let response = session_messages_jsonl_response(json!({
            "messages": [],
            "messages_page": {"limit": 100, "offset": 0, "has_more": false, "next_offset": null}
        }));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get(EXPORT_ROWS_HEADER).unwrap(), "0");

        let body = response_body_string(response).await;

        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn audit_event_jsonl_response_serializes_items_and_headers() {
        let response = audit_event_jsonl_response(json!({
            "items": [
                {"id": 1, "event_type": "login_failed", "user_id": "admin"},
                {"id": 2, "event_type": "logout", "user_id": null}
            ],
            "page": {"limit": 100, "offset": 0, "has_more": false, "next_offset": null}
        }));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            JSONL_CONTENT_TYPE
        );
        assert_eq!(
            response.headers().get(header::CONTENT_DISPOSITION).unwrap(),
            "attachment; filename=\"llmtrace-audit-events.jsonl\""
        );
        assert_eq!(response.headers().get(EXPORT_ROWS_HEADER).unwrap(), "2");

        let body = response_body_string(response).await;
        let lines = body
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(
            lines,
            vec![
                json!({"id": 1, "event_type": "login_failed", "user_id": "admin"}),
                json!({"id": 2, "event_type": "logout", "user_id": null})
            ]
        );
    }

    #[tokio::test]
    async fn audit_event_jsonl_response_allows_empty_exports() {
        let response = audit_event_jsonl_response(json!({
            "items": [],
            "page": {"limit": 100, "offset": 0, "has_more": false, "next_offset": null}
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
    fn request_list_filters_include_api_key_hash() {
        let filters = request_list_filters(RequestListQuery {
            q: None,
            status: None,
            status_class: None,
            has_error: None,
            upstream_host: None,
            model: None,
            request_kind: None,
            session_id: None,
            api_key_hash: Some(" sha256:abc123 ".to_string()),
            since: None,
            until: None,
            min_duration_ms: None,
            max_duration_ms: None,
            limit: None,
            offset: None,
        })
        .unwrap();

        assert_eq!(filters.api_key_hash, Some("sha256:abc123".to_string()));
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
    fn ui_session_hash_normalization_accepts_sha256_hex() {
        let hash = "ABCDEF0123456789abcdef0123456789ABCDEF0123456789abcdef0123456789";

        assert_eq!(
            normalize_ui_session_hash(hash.to_string()).unwrap(),
            hash.to_ascii_lowercase()
        );
    }

    #[test]
    fn ui_session_hash_normalization_rejects_raw_or_invalid_tokens() {
        let short = normalize_ui_session_hash("short-token".to_string()).unwrap_err();
        assert!(short.contains("64-character"));

        let mut invalid = "a".repeat(UI_SESSION_HASH_BYTES);
        invalid.replace_range(10..11, "z");
        let error = normalize_ui_session_hash(invalid).unwrap_err();
        assert!(error.contains("hexadecimal"));
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

    fn posture_check_status<'a>(posture: &'a Value, id: &str) -> Option<&'a str> {
        posture["checks"]
            .as_array()?
            .iter()
            .find(|check| check["id"] == id)?
            .get("status")?
            .as_str()
    }
}
