use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router, extract::State};
use serde_json::{Value, json};

use crate::state::AppState;
use crate::storage;

const SERVICE_NAME: &str = "llmtrace";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
}

async fn healthz() -> Json<Value> {
    Json(json!({
        "service": SERVICE_NAME,
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn readyz(State(state): State<AppState>) -> Response {
    match storage::readiness_check(&state.pool).await {
        Ok(()) => Json(readiness_payload("ready", "ok")).into_response(),
        Err(error) => {
            tracing::warn!(%error, "readiness check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(readiness_payload("not_ready", "unavailable")),
            )
                .into_response()
        }
    }
}

async fn metrics(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(expected_token) = state.config.observability.metrics_bearer_token.as_deref()
        && !bearer_token_authorized(&headers, expected_token)
    {
        return metrics_unauthorized_response();
    }

    let body = state.runtime_metrics.prometheus_text(
        state.pool.size(),
        state.pool.num_idle(),
        state.traces.queue_metrics(),
    );
    let mut response = body.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );
    response
}

fn bearer_token_authorized(headers: &HeaderMap, expected_token: &str) -> bool {
    let Some(header) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some(token) = header.strip_prefix("Bearer ") else {
        return false;
    };
    constant_time_eq(token.as_bytes(), expected_token.as_bytes())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let max_len = left.len().max(right.len());
    let mut diff = left.len() ^ right.len();
    for index in 0..max_len {
        diff |= left.get(index).copied().unwrap_or(0) as usize
            ^ right.get(index).copied().unwrap_or(0) as usize;
    }
    diff == 0
}

fn metrics_unauthorized_response() -> Response {
    let mut response =
        (StatusCode::UNAUTHORIZED, "metrics authentication required").into_response();
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

fn readiness_payload(status: &str, postgres: &str) -> Value {
    json!({
        "service": SERVICE_NAME,
        "status": status,
        "version": env!("CARGO_PKG_VERSION"),
        "checks": {
            "postgres": postgres,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_payload_reports_postgres_check_without_error_details() {
        let payload = readiness_payload("not_ready", "unavailable");

        assert_eq!(payload["service"], "llmtrace");
        assert_eq!(payload["status"], "not_ready");
        assert_eq!(payload["checks"]["postgres"], "unavailable");
        assert!(payload.get("error").is_none());
    }

    #[test]
    fn metrics_bearer_token_accepts_matching_authorization_header() {
        let headers = headers_with_auth("Bearer metrics-secret");

        assert!(bearer_token_authorized(&headers, "metrics-secret"));
    }

    #[test]
    fn metrics_bearer_token_rejects_missing_or_wrong_authorization_header() {
        assert!(!bearer_token_authorized(
            &HeaderMap::new(),
            "metrics-secret"
        ));
        assert!(!bearer_token_authorized(
            &headers_with_auth("Basic metrics-secret"),
            "metrics-secret"
        ));
        assert!(!bearer_token_authorized(
            &headers_with_auth("Bearer other-secret"),
            "metrics-secret"
        ));
    }

    #[test]
    fn metrics_unauthorized_response_advertises_bearer_auth() {
        let response = metrics_unauthorized_response();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response
                .headers()
                .get(header::WWW_AUTHENTICATE)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer")
        );
    }

    fn headers_with_auth(value: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static(value));
        headers
    }
}
