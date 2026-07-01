use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};

use crate::state::AppState;

pub async fn redirect_to_ui(State(state): State<AppState>) -> Response {
    redirect_to_ui_response(state.config.server.ui_enabled)
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

fn redirect_to_ui_response(ui_enabled: bool) -> Response {
    if !ui_enabled {
        return StatusCode::NOT_FOUND.into_response();
    }

    Redirect::temporary("/ui/").into_response()
}

#[cfg(test)]
mod tests {
    use axum::http::header;

    use super::*;

    #[test]
    fn root_redirects_to_ui_when_ui_is_enabled() {
        let response = redirect_to_ui_response(true);

        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("/ui/")
        );
    }

    #[test]
    fn root_returns_not_found_when_ui_is_disabled() {
        let response = redirect_to_ui_response(false);

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(response.headers().get(header::LOCATION).is_none());
    }
}
