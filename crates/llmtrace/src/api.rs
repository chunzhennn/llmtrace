use axum::extract::{Path, Query, State};
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::state::AppState;
use crate::storage;

const MAX_REQUEST_SEARCH_BYTES: usize = 512;
const JSONL_CONTENT_TYPE: &str = "application/x-ndjson; charset=utf-8";
const EXPORT_ROWS_HEADER: HeaderName = HeaderName::from_static("x-llmtrace-export-rows");

#[derive(Debug, Deserialize)]
struct RequestListQuery {
    q: Option<String>,
    status: Option<i32>,
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct SessionListQuery {
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct SessionDetailQuery {
    messages_limit: Option<i64>,
    messages_offset: Option<i64>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/stats", get(stats))
        .route("/requests", get(list_requests))
        .route("/requests/{id}", get(get_request))
        .route("/sessions", get(list_sessions))
        .route("/sessions/{id}", get(get_session))
        .route("/query", post(run_query))
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

async fn list_requests(
    State(state): State<AppState>,
    Query(query): Query<RequestListQuery>,
) -> Response {
    let q = match normalize_request_search(query.q) {
        Ok(q) => q,
        Err(message) => return bad_request(message),
    };

    match storage::list_requests(&state.pool, q, query.status, query.limit.unwrap_or(100)).await {
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
    match storage::list_sessions(&state.pool, query.limit.unwrap_or(100)).await {
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

async fn run_query(
    State(state): State<AppState>,
    Json(payload): Json<storage::StructuredQuery>,
) -> Response {
    match storage::run_structured_query(&state.pool, payload).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => structured_query_error(error),
    }
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

    async fn response_body_json(response: Response) -> serde_json::Value {
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn response_body_string(response: Response) -> String {
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(body.to_vec()).unwrap()
    }
}
