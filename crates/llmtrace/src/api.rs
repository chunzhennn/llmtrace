use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::state::AppState;
use crate::storage;

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
    match storage::list_requests(
        &state.pool,
        query.q,
        query.status,
        query.limit.unwrap_or(100),
    )
    .await
    {
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

async fn plugins(State(state): State<AppState>) -> Response {
    Json(json!({ "items": state.plugins.statuses() })).into_response()
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
        storage::StructuredQueryError::Invalid(message) => {
            (StatusCode::BAD_REQUEST, Json(json!({"error": message}))).into_response()
        }
        storage::StructuredQueryError::Execution(error) => {
            api_error(StatusCode::INTERNAL_SERVER_ERROR, error)
        }
    }
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

    async fn response_body_json(response: Response) -> serde_json::Value {
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }
}
