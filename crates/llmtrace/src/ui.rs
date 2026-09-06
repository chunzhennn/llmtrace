use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use rust_embed::RustEmbed;

use crate::state::AppState;

/// The built SvelteKit SPA (adapter-static, base path `/ui`). In debug builds
/// rust-embed reads these files from disk at runtime; release builds embed them
/// into the binary so the single-binary deploy model is preserved.
#[derive(RustEmbed)]
#[folder = "ui/build"]
#[allow_missing = true]
struct UiAssets;

const INDEX_HTML: &str = "index.html";

pub async fn redirect_to_ui(State(state): State<AppState>) -> Response {
    redirect_to_ui_response(state.config.server.ui_enabled)
}

/// Serves the SPA entry point for `/ui` and `/ui/`.
pub async fn serve_ui_index(State(state): State<AppState>) -> Response {
    if !state.config.server.ui_enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    serve_index()
}

/// Serves an embedded asset by path, falling back to `index.html` so client-side
/// routes (e.g. `/ui/requests/{id}`) resolve to the SPA.
pub async fn serve_ui_path(State(state): State<AppState>, Path(path): Path<String>) -> Response {
    if !state.config.server.ui_enabled {
        return StatusCode::NOT_FOUND.into_response();
    }

    // Reject path traversal (only meaningful for debug on-disk reads).
    if path.split('/').any(|segment| segment == "..") {
        return StatusCode::NOT_FOUND.into_response();
    }

    match UiAssets::get(&path) {
        Some(asset) => asset_response(&path, asset),
        None => serve_index(),
    }
}

fn serve_index() -> Response {
    match UiAssets::get(INDEX_HTML) {
        Some(asset) => asset_response(INDEX_HTML, asset),
        None => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            "<!doctype html><title>llmtrace</title><p>UI has not been built. Run <code>pnpm --dir crates/llmtrace/ui build</code>.</p>",
        )
            .into_response(),
    }
}

fn asset_response(path: &str, asset: rust_embed::EmbeddedFile) -> Response {
    let mime = mime_for(path, &asset);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, mime)],
        asset.data.into_owned(),
    )
        .into_response()
}

fn mime_for(path: &str, asset: &rust_embed::EmbeddedFile) -> String {
    // Prefer the mimetype rust-embed guessed at build time; fall back to the
    // extension so newly-added asset kinds still get a sensible content type.
    let guessed = asset.metadata.mimetype();
    if !guessed.is_empty() && guessed != "application/octet-stream" {
        return guessed.to_string();
    }
    mime_from_extension(path).to_string()
}

fn mime_from_extension(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
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

    #[tokio::test]
    async fn index_page_is_html_with_or_without_a_frontend_build() {
        let response = serve_index();
        let status = response.status();
        assert!(matches!(status, StatusCode::OK | StatusCode::NOT_FOUND));
        assert!(
            response.headers()[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/html")
        );
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.to_ascii_lowercase().contains("<!doctype html>"));
        if status == StatusCode::NOT_FOUND {
            assert!(body.contains("UI has not been built"));
        }
    }

    #[test]
    fn mime_from_extension_maps_common_asset_types() {
        assert_eq!(
            mime_from_extension("app/index.html"),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            mime_from_extension("_app/immutable/x.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            mime_from_extension("_app/immutable/x.css"),
            "text/css; charset=utf-8"
        );
        assert_eq!(mime_from_extension("noext"), "application/octet-stream");
    }
}
