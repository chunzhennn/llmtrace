use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};

use crate::state::AppState;

pub async fn redirect_to_ui() -> Redirect {
    Redirect::temporary("/ui/")
}

pub async fn serve_ui(State(state): State<AppState>) -> Response {
    if !state.config.server.ui_enabled {
        return StatusCode::NOT_FOUND.into_response();
    }

    (
        StatusCode::NOT_FOUND,
        "llmtrace UI has been removed from this repository. Mount or implement a frontend separately.",
    )
        .into_response()
}
