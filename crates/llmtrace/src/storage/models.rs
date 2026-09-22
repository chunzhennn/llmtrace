//! Typed read models shared by API projections and session exports.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct RequestSummary {
    pub id: Uuid,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub method: String,
    pub original_uri: String,
    pub upstream_url: String,
    pub upstream_host: Option<String>,
    pub status: Option<i32>,
    pub error: Option<String>,
    pub request_kind: String,
    pub model: Option<String>,
    pub api_key_hash: Option<String>,
    pub session_id: Option<Uuid>,
    pub ttft_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub bytes_in: i64,
    pub bytes_out: i64,
    pub request_body_truncated: bool,
    pub response_body_truncated: bool,
    pub plugin_metadata: Value,
    pub tags: Vec<String>,
    pub ttfb_ms: Option<i64>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cached_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
    pub estimated_cost_microusd: Option<i64>,
    pub usage_complete: bool,
    pub tool_call_count: i64,
}

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct RequestMetadata {
    #[serde(flatten)]
    #[sqlx(flatten)]
    pub summary: RequestSummary,
    pub session_key: Option<String>,
    pub request_headers: Value,
    pub response_headers: Value,
    pub tool_calls: Value,
    pub request_body_bytes: i64,
    pub response_body_bytes: i64,
    pub content_type: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RequestDetail {
    #[serde(flatten)]
    pub request: RequestMetadata,
    pub bodies_included: bool,
    pub request_body: String,
    pub response_body: String,
    pub request_body_status: Option<BodyStatus>,
    pub response_body_status: Option<BodyStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BodyStatus {
    Available,
    Missing,
    Unreadable,
}

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct SessionInfo {
    pub id: Uuid,
    pub session_key: String,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub user_id: Option<String>,
    pub user_name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct SessionSummary {
    #[serde(flatten)]
    #[sqlx(flatten)]
    pub info: SessionInfo,
    pub request_count: i64,
    pub max_duration_ms: i64,
}

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct SessionMetadata {
    #[serde(flatten)]
    #[sqlx(flatten)]
    pub info: SessionInfo,
    pub summary: Value,
}

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct SessionRequestStats {
    pub request_count: i64,
    pub error_count: i64,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub estimated_cost_microusd: Option<i64>,
    pub tool_call_count: i64,
    pub usage_known_count: i64,
    pub priced_request_count: i64,
    pub incomplete_capture_count: i64,
    pub bytes_in: i64,
    pub bytes_out: i64,
    pub captured_bytes: i64,
    pub avg_duration_ms: Option<i64>,
    pub max_duration_ms: Option<i64>,
    pub avg_ttft_ms: Option<i64>,
    pub max_ttft_ms: Option<i64>,
    pub first_request_at: Option<DateTime<Utc>>,
    pub last_request_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct SessionMessage {
    pub id: i64,
    pub request_id: Uuid,
    /// None for legacy previews whose individual truncation status was not recorded.
    pub content_truncated: Option<bool>,
    pub role: String,
    pub content: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionDetail {
    #[serde(flatten)]
    pub session: SessionMetadata,
    pub request_stats: SessionRequestStats,
    pub messages: Vec<SessionMessage>,
    pub messages_page: PageInfo,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PageInfo {
    pub limit: i64,
    pub offset: i64,
    pub has_more: bool,
    pub next_offset: Option<i64>,
}

pub(super) const REQUEST_METADATA_COLUMNS: &str = "id, started_at, completed_at, method, original_uri, upstream_url, upstream_host, status, error, request_kind, model, api_key_hash, session_id, ttft_ms, duration_ms, bytes_in, bytes_out, request_body_truncated, response_body_truncated, plugin_metadata, tags, ttfb_ms, input_tokens, output_tokens, cached_input_tokens, cache_creation_input_tokens, estimated_cost_microusd, usage_complete, tool_call_count, session_key, request_headers, response_headers, tool_calls, request_body_bytes, response_body_bytes, content_type";

#[derive(Debug, FromRow)]
pub(super) struct ErrorRateCounts {
    pub request_count: i64,
    pub error_count: i64,
}
impl Serialize for ErrorRateCounts {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("ErrorRateCounts", 3)?;
        state.serialize_field("request_count", &self.request_count)?;
        state.serialize_field("error_count", &self.error_count)?;
        state.serialize_field(
            "error_rate",
            &super::rate(self.error_count, self.request_count),
        )?;
        state.end()
    }
}

#[derive(Debug, Serialize, FromRow)]
pub(super) struct NamedMetric {
    pub name: String,
    pub request_count: i64,
    pub error_count: i64,
    pub avg_duration_ms: Option<i64>,
    pub avg_ttft_ms: Option<i64>,
}

#[derive(Debug, Serialize, FromRow)]
pub(super) struct ErrorMetric {
    pub name: String,
    pub error_count: i64,
    pub proxy_error_count: i64,
    pub http_5xx_count: i64,
    pub avg_duration_ms: Option<i64>,
    pub max_duration_ms: Option<i64>,
}

#[derive(Debug, Serialize, FromRow)]
pub(super) struct LatencyMetrics {
    pub request_count: i64,
    pub duration_count: i64,
    pub avg_duration_ms: Option<i64>,
    pub max_duration_ms: Option<i64>,
    pub p50_duration_ms: Option<i64>,
    pub p90_duration_ms: Option<i64>,
    pub p95_duration_ms: Option<i64>,
    pub p99_duration_ms: Option<i64>,
    pub ttft_count: i64,
    pub avg_ttft_ms: Option<i64>,
    pub max_ttft_ms: Option<i64>,
    pub p50_ttft_ms: Option<i64>,
    pub p90_ttft_ms: Option<i64>,
    pub p95_ttft_ms: Option<i64>,
    pub p99_ttft_ms: Option<i64>,
}

#[derive(Debug, Serialize, FromRow)]
pub(super) struct NamedLatencyMetrics {
    #[serde(flatten)]
    #[sqlx(flatten)]
    pub metrics: LatencyMetrics,
    pub name: String,
}

#[derive(Debug, Serialize, FromRow)]
pub(super) struct ApiKeyUsage {
    pub api_key_hash: String,
    pub request_count: i64,
    pub error_count: i64,
    pub session_count: i64,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub estimated_cost_microusd: Option<i64>,
    pub tool_call_count: Option<i64>,
    pub usage_known_count: i64,
    pub priced_request_count: i64,
    pub bytes_in: i64,
    pub bytes_out: i64,
    pub captured_bytes: i64,
    pub avg_duration_ms: Option<i64>,
    pub max_duration_ms: Option<i64>,
    pub avg_ttft_ms: Option<i64>,
    pub max_ttft_ms: Option<i64>,
    pub first_seen_at: Option<DateTime<Utc>>,
    pub last_seen_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, FromRow)]
pub(super) struct UpstreamHealth {
    #[serde(flatten)]
    #[sqlx(flatten)]
    pub counts: ErrorRateCounts,
    pub upstream_host: String,
    pub proxy_error_count: i64,
    pub http_2xx_count: i64,
    pub http_3xx_count: i64,
    pub http_4xx_count: i64,
    pub http_5xx_count: i64,
    pub no_status_count: i64,
    pub session_count: i64,
    pub bytes_in: i64,
    pub bytes_out: i64,
    pub captured_bytes: i64,
    pub duration_count: i64,
    pub avg_duration_ms: Option<i64>,
    pub max_duration_ms: Option<i64>,
    pub p95_duration_ms: Option<i64>,
    pub ttft_count: i64,
    pub avg_ttft_ms: Option<i64>,
    pub max_ttft_ms: Option<i64>,
    pub p95_ttft_ms: Option<i64>,
    pub first_seen_at: Option<DateTime<Utc>>,
    pub last_seen_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, FromRow)]
pub(super) struct ModelUsage {
    #[serde(flatten)]
    #[sqlx(flatten)]
    pub counts: ErrorRateCounts,
    pub model: String,
    pub proxy_error_count: i64,
    pub http_5xx_count: i64,
    pub upstream_count: i64,
    pub api_key_count: i64,
    pub session_count: i64,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub estimated_cost_microusd: Option<i64>,
    pub tool_call_count: Option<i64>,
    pub usage_known_count: i64,
    pub priced_request_count: i64,
    pub bytes_in: i64,
    pub bytes_out: i64,
    pub captured_bytes: i64,
    pub duration_count: i64,
    pub avg_duration_ms: Option<i64>,
    pub max_duration_ms: Option<i64>,
    pub p95_duration_ms: Option<i64>,
    pub ttft_count: i64,
    pub avg_ttft_ms: Option<i64>,
    pub max_ttft_ms: Option<i64>,
    pub p95_ttft_ms: Option<i64>,
    pub first_seen_at: Option<DateTime<Utc>>,
    pub last_seen_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, FromRow)]
pub(super) struct UserUsage {
    #[serde(flatten)]
    #[sqlx(flatten)]
    pub counts: ErrorRateCounts,
    pub user_id: String,
    pub user_name: String,
    pub proxy_error_count: i64,
    pub http_5xx_count: i64,
    pub session_count: i64,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub estimated_cost_microusd: Option<i64>,
    pub tool_call_count: Option<i64>,
    pub usage_known_count: i64,
    pub priced_request_count: i64,
    pub api_key_count: i64,
    pub upstream_count: i64,
    pub bytes_in: i64,
    pub bytes_out: i64,
    pub captured_bytes: i64,
    pub duration_count: i64,
    pub avg_duration_ms: Option<i64>,
    pub max_duration_ms: Option<i64>,
    pub p95_duration_ms: Option<i64>,
    pub ttft_count: i64,
    pub avg_ttft_ms: Option<i64>,
    pub max_ttft_ms: Option<i64>,
    pub p95_ttft_ms: Option<i64>,
    pub first_seen_at: Option<DateTime<Utc>>,
    pub last_seen_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, FromRow)]
pub(super) struct AuditSummaryRow {
    pub name: String,
    pub event_count: i64,
    pub user_count: i64,
    pub remote_addr_count: i64,
    pub first_seen_at: Option<DateTime<Utc>>,
    pub last_seen_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, FromRow)]
pub(super) struct UsagePoint {
    pub bucket: DateTime<Utc>,
    pub request_count: i64,
    pub error_count: i64,
    pub captured_bytes: i64,
    pub avg_duration_ms: Option<i64>,
    pub avg_ttft_ms: Option<i64>,
}

#[derive(Debug, Serialize, FromRow)]
pub(super) struct AuditEvent {
    pub id: i64,
    pub created_at: DateTime<Utc>,
    pub event_type: String,
    pub user_id: Option<String>,
    pub remote_addr: Option<String>,
    pub detail: Value,
}

#[derive(Serialize, FromRow)]
pub(super) struct Facet<T> {
    pub value: T,
    pub request_count: i64,
}

#[derive(Serialize, FromRow)]
pub(super) struct NamedCount {
    pub name: String,
    pub request_count: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_contract_matches_frontend_fixture() {
        let expected: Value = serde_json::from_str(include_str!(
            "../../ui/src/lib/api/fixtures/request-detail.json"
        ))
        .unwrap();
        let response: RequestDetail = serde_json::from_value(expected.clone()).unwrap();
        assert_eq!(serde_json::to_value(response).unwrap(), expected);
    }

    #[test]
    fn session_contract_matches_frontend_fixture() {
        let expected: Value = serde_json::from_str(include_str!(
            "../../ui/src/lib/api/fixtures/session-detail.json"
        ))
        .unwrap();
        let response: SessionDetail = serde_json::from_value(expected.clone()).unwrap();
        assert_eq!(serde_json::to_value(response).unwrap(), expected);
    }

    #[test]
    fn session_message_contract_matches_frontend_fixture() {
        let expected: Value = serde_json::from_str(include_str!(
            "../../ui/src/lib/api/fixtures/session-message.json"
        ))
        .unwrap();
        let response: SessionMessage = serde_json::from_value(expected.clone()).unwrap();
        assert_eq!(serde_json::to_value(response).unwrap(), expected);
    }
}
