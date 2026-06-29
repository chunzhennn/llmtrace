use axum::http::StatusCode;
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
}
