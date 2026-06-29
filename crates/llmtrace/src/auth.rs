use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordVerifier};
use axum::body::Body;
use axum::extract::connect_info::ConnectInfo;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{Duration, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;
use std::net::SocketAddr;
use std::time::Duration as StdDuration;
use url::Url;

use crate::config::OAuthConfig;
use crate::login_throttle::LoginThrottleDecision;
use crate::state::AppState;
use crate::types::LoginMethod;

const SESSION_COOKIE: &str = "llmtrace_session";

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MeResponse {
    pub user_id: String,
    pub display_name: String,
    pub login_method: LoginMethod,
}

#[derive(Debug, Deserialize)]
struct OAuthCallback {
    code: String,
    state: String,
}

#[derive(Debug, Deserialize)]
struct OidcMetadata {
    authorization_endpoint: Option<String>,
    token_endpoint: Option<String>,
    userinfo_endpoint: Option<String>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/me", get(me))
        .route("/oauth/start", get(oauth_start))
        .route("/oauth/callback", get(oauth_callback))
}

pub async fn require_auth(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    if !same_origin_request_allowed(
        &state.config.server.public_url,
        request.method(),
        request.headers(),
    ) {
        return cross_site_request_response(request.method());
    }

    match current_user(&state, request.headers()).await {
        Ok(Some(user)) => {
            request.extensions_mut().insert(user);
            next.run(request).await
        }
        Ok(None) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "login required"})),
        )
            .into_response(),
        Err(error) => server_error(error),
    }
}

async fn login(
    State(state): State<AppState>,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(payload): Json<LoginRequest>,
) -> Response {
    if !same_origin_request_allowed(&state.config.server.public_url, &Method::POST, &headers) {
        return cross_site_request_response(&Method::POST);
    }

    let remote_ip = remote_addr.ip().to_string();
    match state.login_throttle.check(&payload.username, &remote_ip) {
        LoginThrottleDecision::Allowed => {}
        LoginThrottleDecision::Limited { retry_after } => {
            let _ = audit(
                &state,
                "login_throttled",
                Some(payload.username),
                Some(remote_ip),
                json!({"retry_after_secs": retry_after_secs(retry_after)}),
            )
            .await;
            return throttled_response(retry_after);
        }
    }

    let admin = &state.config.auth.local_admin;
    let valid_user = payload.username == admin.username;
    let valid_password = valid_user
        && verify_password(
            &payload.password,
            admin.password_hash.as_deref(),
            admin.password.as_deref(),
        );

    if !valid_password {
        let retry_after = state
            .login_throttle
            .record_failure(&payload.username, &remote_ip);
        let _ = audit(
            &state,
            "login_failed",
            Some(payload.username),
            Some(remote_ip),
            retry_after
                .map(|retry_after| {
                    json!({
                        "throttled": true,
                        "retry_after_secs": retry_after_secs(retry_after),
                    })
                })
                .unwrap_or_else(|| json!({})),
        )
        .await;
        return retry_after
            .map(throttled_response)
            .unwrap_or_else(invalid_login_response);
    }

    state
        .login_throttle
        .record_success(&payload.username, &remote_ip);
    match create_session(&state, &admin.username, &admin.username, LoginMethod::Local).await {
        Ok(session_id) => {
            let _ = audit(
                &state,
                "login_ok",
                Some(admin.username.clone()),
                Some(remote_ip),
                json!({"method": LoginMethod::Local.as_str()}),
            )
            .await;
            let mut response = Json(json!({"ok": true})).into_response();
            set_session_cookie(response.headers_mut(), &state, &session_id);
            response
        }
        Err(error) => server_error(error),
    }
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !same_origin_request_allowed(&state.config.server.public_url, &Method::POST, &headers) {
        return cross_site_request_response(&Method::POST);
    }

    if let Some(session_id) = session_cookie(&headers) {
        let _ = sqlx::query("DELETE FROM ui_sessions WHERE id = $1")
            .bind(&session_id)
            .execute(&state.pool)
            .await;
        let _ = audit(&state, "logout", None, None, json!({})).await;
    }
    let mut response = Json(json!({"ok": true})).into_response();
    clear_session_cookie(response.headers_mut(), &state);
    response
}

async fn me(State(state): State<AppState>, headers: HeaderMap) -> Response {
    match current_user(&state, &headers).await {
        Ok(Some(user)) => Json(json!({
            "authenticated": true,
            "user": user
        }))
        .into_response(),
        Ok(None) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"authenticated": false})),
        )
            .into_response(),
        Err(error) => server_error(error),
    }
}

async fn oauth_start(State(state): State<AppState>) -> Response {
    let oauth = &state.config.auth.oauth;
    if !oauth.enabled {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "oauth is disabled"})),
        )
            .into_response();
    }
    if oauth.client_id.is_empty() || oauth.issuer_url.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "oauth issuer_url and client_id are required"})),
        )
            .into_response();
    }

    let Ok(metadata) = oauth_metadata(&state).await else {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": "failed to discover oauth provider"})),
        )
            .into_response();
    };
    let Some(auth_url) = metadata.authorization_endpoint else {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": "oauth provider has no authorization endpoint"})),
        )
            .into_response();
    };
    let state_value = random_token();
    let expires_at = Utc::now() + Duration::minutes(10);
    if let Err(error) = sqlx::query(
        "INSERT INTO oauth_states (state, created_at, expires_at) VALUES ($1, now(), $2)",
    )
    .bind(&state_value)
    .bind(expires_at)
    .execute(&state.pool)
    .await
    {
        return server_error(error);
    }

    let redirect_url = oauth_redirect_url(&state);
    let location = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope=openid%20email%20profile&state={}",
        auth_url,
        urlencoding(&oauth.client_id),
        urlencoding(&redirect_url),
        urlencoding(&state_value),
    );
    Redirect::temporary(&location).into_response()
}

async fn oauth_callback(
    State(state): State<AppState>,
    Query(callback): Query<OAuthCallback>,
) -> Response {
    let state_row = match sqlx::query(
        "DELETE FROM oauth_states WHERE state = $1 AND expires_at > now() RETURNING state",
    )
    .bind(&callback.state)
    .fetch_optional(&state.pool)
    .await
    {
        Ok(row) => row,
        Err(error) => return server_error(error),
    };
    if state_row.is_none() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "invalid oauth state"})),
        )
            .into_response();
    }

    match finish_oauth(&state, &callback.code).await {
        Ok(user) => {
            match create_session(
                &state,
                &user.user_id,
                &user.display_name,
                LoginMethod::OAuth,
            )
            .await
            {
                Ok(session_id) => {
                    let _ = audit(
                        &state,
                        "login_ok",
                        Some(user.user_id),
                        None,
                        json!({"method": LoginMethod::OAuth.as_str()}),
                    )
                    .await;
                    let mut response = Redirect::temporary("/ui/").into_response();
                    set_session_cookie(response.headers_mut(), &state, &session_id);
                    response
                }
                Err(error) => server_error(error),
            }
        }
        Err(error) => {
            tracing::warn!(%error, "oauth callback failed");
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "oauth login failed"})),
            )
                .into_response()
        }
    }
}

async fn finish_oauth(state: &AppState, code: &str) -> anyhow::Result<MeResponse> {
    let oauth = &state.config.auth.oauth;
    let metadata = oauth_metadata(state).await?;
    let token_endpoint = metadata
        .token_endpoint
        .ok_or_else(|| anyhow::anyhow!("oauth provider has no token endpoint"))?;
    let userinfo_endpoint = metadata
        .userinfo_endpoint
        .ok_or_else(|| anyhow::anyhow!("oauth provider has no userinfo endpoint"))?;
    let redirect_url = oauth_redirect_url(state);

    let token_response: Value = state
        .http
        .post(token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", &redirect_url),
            ("client_id", &oauth.client_id),
            ("client_secret", &oauth.client_secret),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let access_token = token_response
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("oauth token response missing access_token"))?;

    let userinfo: Value = state
        .http
        .get(userinfo_endpoint)
        .bearer_auth(access_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let email = userinfo
        .get("email")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("oauth userinfo missing email"))?
        .to_ascii_lowercase();
    enforce_oauth_email_verified(oauth, &userinfo)?;
    enforce_oauth_allowlist(oauth, &email)?;
    let display_name = userinfo
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(&email)
        .to_string();

    Ok(MeResponse {
        user_id: email,
        display_name,
        login_method: LoginMethod::OAuth,
    })
}

async fn oauth_metadata(state: &AppState) -> anyhow::Result<OidcMetadata> {
    let issuer = state.config.auth.oauth.issuer_url.trim_end_matches('/');
    let discovery = format!("{issuer}/.well-known/openid-configuration");
    match state.http.get(discovery).send().await {
        Ok(response) if response.status().is_success() => Ok(response.json().await?),
        _ => Ok(OidcMetadata {
            authorization_endpoint: Some(format!("{issuer}/authorize")),
            token_endpoint: Some(format!("{issuer}/token")),
            userinfo_endpoint: Some(format!("{issuer}/userinfo")),
        }),
    }
}

fn enforce_oauth_email_verified(oauth: &OAuthConfig, userinfo: &Value) -> anyhow::Result<()> {
    if !oauth.require_email_verified {
        return Ok(());
    }

    match userinfo.get("email_verified").and_then(Value::as_bool) {
        Some(true) => Ok(()),
        Some(false) => anyhow::bail!("oauth email is not verified"),
        None => anyhow::bail!("oauth userinfo missing true email_verified claim"),
    }
}

fn enforce_oauth_allowlist(oauth: &OAuthConfig, email: &str) -> anyhow::Result<()> {
    if !oauth.allowed_emails.is_empty()
        && oauth
            .allowed_emails
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(email))
    {
        return Ok(());
    }
    if !oauth.allowed_domains.is_empty()
        && let Some((_, domain)) = email.rsplit_once('@')
        && oauth
            .allowed_domains
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(domain))
    {
        return Ok(());
    }
    if oauth.allowed_emails.is_empty() && oauth.allowed_domains.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("oauth account is not allowed")
    }
}

async fn current_user(state: &AppState, headers: &HeaderMap) -> anyhow::Result<Option<MeResponse>> {
    let Some(session_id) = session_cookie(headers) else {
        return Ok(None);
    };
    let row = sqlx::query(
        r#"
        SELECT user_id, display_name, login_method
        FROM ui_sessions
        WHERE id = $1 AND expires_at > now()
        "#,
    )
    .bind(session_id)
    .fetch_optional(&state.pool)
    .await?;

    row.map(|row| {
        Ok(MeResponse {
            user_id: row.get("user_id"),
            display_name: row.get("display_name"),
            login_method: row.get::<String, _>("login_method").parse()?,
        })
    })
    .transpose()
}

fn verify_password(password: &str, hash: Option<&str>, development_password: Option<&str>) -> bool {
    if let Some(hash) = hash
        && let Ok(parsed) = PasswordHash::new(hash)
    {
        return Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok();
    }
    development_password.is_some_and(|expected| expected == password)
}

async fn create_session(
    state: &AppState,
    user_id: &str,
    display_name: &str,
    login_method: LoginMethod,
) -> anyhow::Result<String> {
    let session_id = random_token();
    let expires_at = Utc::now() + Duration::hours(state.config.auth.session_ttl_hours);
    sqlx::query(
        r#"
        INSERT INTO ui_sessions (id, user_id, display_name, login_method, created_at, expires_at)
        VALUES ($1, $2, $3, $4, now(), $5)
        "#,
    )
    .bind(&session_id)
    .bind(user_id)
    .bind(display_name)
    .bind(login_method.as_str())
    .bind(expires_at)
    .execute(&state.pool)
    .await?;
    Ok(session_id)
}

async fn audit(
    state: &AppState,
    event_type: &str,
    user_id: Option<String>,
    remote_addr: Option<String>,
    detail: Value,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO ui_audit_events (event_type, user_id, remote_addr, detail)
        VALUES ($1, $2, $3, $4)
        "#,
    )
    .bind(event_type)
    .bind(user_id)
    .bind(remote_addr)
    .bind(detail)
    .execute(&state.pool)
    .await?;
    Ok(())
}

fn session_cookie(headers: &HeaderMap) -> Option<String> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in cookie.split(';') {
        let (name, value) = part.trim().split_once('=')?;
        if name == SESSION_COOKIE {
            return Some(value.to_string());
        }
    }
    None
}

fn cross_site_request_response(method: &Method) -> Response {
    tracing::warn!(method = %method, "cross-site request rejected");
    (
        StatusCode::FORBIDDEN,
        Json(json!({"error": "cross-site request rejected"})),
    )
        .into_response()
}

fn same_origin_request_allowed(public_url: &str, method: &Method, headers: &HeaderMap) -> bool {
    if safe_method(method) {
        return true;
    }

    if let Some(origin) = header_to_str(headers, header::ORIGIN) {
        return origin_matches_public_url(public_url, origin);
    }

    header_to_str(headers, header::REFERER)
        .map(|referer| origin_matches_public_url(public_url, referer))
        .unwrap_or(true)
}

fn safe_method(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
    )
}

fn header_to_str(headers: &HeaderMap, name: axum::http::HeaderName) -> Option<&str> {
    headers.get(name)?.to_str().ok()
}

fn origin_matches_public_url(public_url: &str, candidate: &str) -> bool {
    let Ok(public_url) = Url::parse(public_url) else {
        return false;
    };
    let Ok(candidate) = Url::parse(candidate) else {
        return false;
    };

    public_url.scheme() == candidate.scheme()
        && public_url.host_str().zip(candidate.host_str()).is_some_and(
            |(public_host, candidate_host)| public_host.eq_ignore_ascii_case(candidate_host),
        )
        && public_url.port_or_known_default() == candidate.port_or_known_default()
}

fn set_session_cookie(headers: &mut HeaderMap, state: &AppState, session_id: &str) {
    let mut cookie = format!(
        "{SESSION_COOKIE}={session_id}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}",
        state.config.auth.session_ttl_hours * 3600
    );
    if state.config.auth.cookie_secure {
        cookie.push_str("; Secure");
    }
    headers.insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
}

fn clear_session_cookie(headers: &mut HeaderMap, state: &AppState) {
    let mut cookie = format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    if state.config.auth.cookie_secure {
        cookie.push_str("; Secure");
    }
    headers.insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
}

fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn oauth_redirect_url(state: &AppState) -> String {
    let configured = state.config.auth.oauth.redirect_url.trim();
    if !configured.is_empty() {
        return configured.to_string();
    }
    format!(
        "{}/api/auth/oauth/callback",
        state.config.server.public_url.trim_end_matches('/')
    )
}

fn urlencoding(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn invalid_login_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": "invalid username or password"})),
    )
        .into_response()
}

fn throttled_response(retry_after: StdDuration) -> Response {
    let retry_after_secs = retry_after_secs(retry_after);
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({
            "error": "too many login attempts",
            "retry_after_secs": retry_after_secs,
        })),
    )
        .into_response();
    if let Ok(value) = HeaderValue::from_str(&retry_after_secs.to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

fn retry_after_secs(retry_after: StdDuration) -> u64 {
    retry_after.as_secs().max(1)
}

fn server_error(error: impl std::fmt::Display) -> Response {
    tracing::error!(%error, "auth request failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": "internal server error"})),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUBLIC_URL: &str = "https://llmtrace.example.com";

    #[test]
    fn oauth_email_verification_accepts_true_claim() {
        let oauth = OAuthConfig::default();

        enforce_oauth_email_verified(&oauth, &json!({"email_verified": true})).unwrap();
    }

    #[test]
    fn oauth_email_verification_rejects_false_claim() {
        let oauth = OAuthConfig::default();

        let error = enforce_oauth_email_verified(&oauth, &json!({"email_verified": false}))
            .unwrap_err()
            .to_string();

        assert!(error.contains("oauth email is not verified"));
    }

    #[test]
    fn oauth_email_verification_rejects_missing_claim_when_required() {
        let oauth = OAuthConfig::default();

        let error = enforce_oauth_email_verified(&oauth, &json!({}))
            .unwrap_err()
            .to_string();

        assert!(error.contains("missing true email_verified"));
    }

    #[test]
    fn oauth_email_verification_can_be_disabled_outside_production() {
        let oauth = OAuthConfig {
            require_email_verified: false,
            ..OAuthConfig::default()
        };

        enforce_oauth_email_verified(&oauth, &json!({})).unwrap();
    }

    #[test]
    fn same_origin_check_allows_safe_method_with_cross_site_origin() {
        let headers = headers_with(header::ORIGIN, "https://evil.example.com");

        assert!(same_origin_request_allowed(
            PUBLIC_URL,
            &Method::GET,
            &headers
        ));
    }

    #[test]
    fn same_origin_check_allows_unsafe_method_without_browser_origin_headers() {
        let headers = HeaderMap::new();

        assert!(same_origin_request_allowed(
            PUBLIC_URL,
            &Method::POST,
            &headers
        ));
    }

    #[test]
    fn same_origin_check_allows_matching_origin_for_unsafe_method() {
        let headers = headers_with(header::ORIGIN, "https://llmtrace.example.com");

        assert!(same_origin_request_allowed(
            PUBLIC_URL,
            &Method::POST,
            &headers
        ));
    }

    #[test]
    fn same_origin_check_rejects_cross_site_origin_for_unsafe_method() {
        let headers = headers_with(header::ORIGIN, "https://evil.example.com");

        assert!(!same_origin_request_allowed(
            PUBLIC_URL,
            &Method::POST,
            &headers
        ));
    }

    #[test]
    fn same_origin_check_uses_referer_when_origin_is_absent() {
        let headers = headers_with(
            header::REFERER,
            "https://llmtrace.example.com/ui/settings?tab=auth",
        );

        assert!(same_origin_request_allowed(
            PUBLIC_URL,
            &Method::DELETE,
            &headers
        ));
    }

    #[test]
    fn same_origin_check_rejects_cross_site_referer_when_origin_is_absent() {
        let headers = headers_with(header::REFERER, "https://evil.example.com/form");

        assert!(!same_origin_request_allowed(
            PUBLIC_URL,
            &Method::DELETE,
            &headers
        ));
    }

    #[test]
    fn same_origin_check_rejects_null_origin_for_unsafe_method() {
        let headers = headers_with(header::ORIGIN, "null");

        assert!(!same_origin_request_allowed(
            PUBLIC_URL,
            &Method::POST,
            &headers
        ));
    }

    #[test]
    fn same_origin_check_origin_takes_precedence_over_referer() {
        let mut headers = headers_with(header::ORIGIN, "https://llmtrace.example.com");
        headers.insert(
            header::REFERER,
            HeaderValue::from_static("https://evil.example.com/form"),
        );

        assert!(same_origin_request_allowed(
            PUBLIC_URL,
            &Method::POST,
            &headers
        ));
    }

    fn headers_with(name: axum::http::HeaderName, value: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(name, HeaderValue::from_static(value));
        headers
    }
}
