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
use futures_util::StreamExt;
use rand::RngCore;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;
use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration as StdDuration;
use url::Url;

use crate::config::{LocalAdminConfig, OAuthConfig};
use crate::login_throttle::LoginThrottleDecision;
use crate::state::AppState;
use crate::types::LoginMethod;

const SESSION_COOKIE: &str = "llmtrace_session";
const OAUTH_STATE_COOKIE: &str = "llmtrace_oauth_state";
const OAUTH_STATE_TTL_SECS: i64 = 10 * 60;
const OAUTH_JSON_BODY_LIMIT_BYTES: usize = 64 * 1024;
const MAX_LOGIN_USERNAME_BYTES: usize = 320;
const MAX_LOGIN_PASSWORD_BYTES: usize = 4096;
const MAX_SESSION_IDENTITY_BYTES: usize = 1024;
const MAX_AUDIT_TEXT_BYTES: usize = 1024;
const AUTH_TOKEN_BYTES: usize = 32;
const AUTH_TOKEN_ENCODED_LEN: usize = 43;
const AUTH_DB_TIMEOUT: StdDuration = StdDuration::from_secs(5);

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

    if let Some(reason) = login_payload_size_error(&payload) {
        let retry_after = state
            .login_throttle
            .record_failure(&payload.username, &remote_ip);
        let _ = audit(
            &state,
            "login_failed",
            Some(payload.username),
            Some(remote_ip),
            login_failure_detail(retry_after, Some(reason)),
        )
        .await;
        return retry_after
            .map(throttled_response)
            .unwrap_or_else(invalid_login_response);
    }

    let admin = state.config.auth.local_admin.clone();
    let valid_password = match verify_local_admin_credentials_blocking(
        admin.clone(),
        payload.username.clone(),
        payload.password.clone(),
    )
    .await
    {
        Ok(valid) => valid,
        Err(error) => return server_error(error),
    };

    if !valid_password {
        let retry_after = state
            .login_throttle
            .record_failure(&payload.username, &remote_ip);
        let _ = audit(
            &state,
            "login_failed",
            Some(payload.username),
            Some(remote_ip),
            login_failure_detail(retry_after, None),
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
        let _ = auth_db_timeout("session delete", async {
            sqlx::query("DELETE FROM ui_sessions WHERE id = $1")
                .bind(&session_id)
                .execute(&state.pool)
                .await?;
            Ok(())
        })
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
    if let Err(error) = auth_db_timeout("oauth state insert", async {
        sqlx::query(
            "INSERT INTO oauth_states (state, created_at, expires_at) VALUES ($1, now(), $2)",
        )
        .bind(&state_value)
        .bind(expires_at)
        .execute(&state.pool)
        .await?;
        Ok(())
    })
    .await
    {
        return server_error(error);
    }

    let redirect_url = oauth_redirect_url(&state);
    let location = match oauth_authorization_location(
        &auth_url,
        &oauth.client_id,
        &redirect_url,
        &state_value,
    ) {
        Ok(location) => location,
        Err(error) => {
            tracing::warn!(%error, "failed to build oauth authorization URL");
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": "oauth provider has an invalid authorization endpoint"})),
            )
                .into_response();
        }
    };
    let mut response = Redirect::temporary(&location).into_response();
    set_oauth_state_cookie(response.headers_mut(), &state, &state_value);
    response
}

async fn oauth_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(callback): Query<OAuthCallback>,
) -> Response {
    if oauth_state_cookie(&headers).as_deref() != Some(callback.state.as_str()) {
        let mut response = (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "invalid oauth state"})),
        )
            .into_response();
        clear_oauth_state_cookie(response.headers_mut(), &state);
        return response;
    }

    let state_row = match auth_db_timeout("oauth state consume", async {
        let row = sqlx::query(
            "DELETE FROM oauth_states WHERE state = $1 AND expires_at > now() RETURNING state",
        )
        .bind(&callback.state)
        .fetch_optional(&state.pool)
        .await?;
        Ok(row)
    })
    .await
    {
        Ok(row) => row,
        Err(error) => return server_error(error),
    };
    if state_row.is_none() {
        let mut response = (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "invalid oauth state"})),
        )
            .into_response();
        clear_oauth_state_cookie(response.headers_mut(), &state);
        return response;
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
                    clear_oauth_state_cookie(response.headers_mut(), &state);
                    response
                }
                Err(error) => {
                    let mut response = server_error(error);
                    clear_oauth_state_cookie(response.headers_mut(), &state);
                    response
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "oauth callback failed");
            let mut response = (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "oauth login failed"})),
            )
                .into_response();
            clear_oauth_state_cookie(response.headers_mut(), &state);
            response
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

    let token_response = state
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
        .error_for_status()?;
    let token_response: Value = read_oauth_json(token_response, "oauth token response").await?;
    let access_token = token_response
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("oauth token response missing access_token"))?;

    let userinfo = state
        .http
        .get(userinfo_endpoint)
        .bearer_auth(access_token)
        .send()
        .await?
        .error_for_status()?;
    let userinfo: Value = read_oauth_json(userinfo, "oauth userinfo response").await?;

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
    validate_identity_field("oauth email", &email)?;
    validate_identity_field("oauth display name", &display_name)?;

    Ok(MeResponse {
        user_id: email,
        display_name,
        login_method: LoginMethod::OAuth,
    })
}

async fn oauth_metadata(state: &AppState) -> anyhow::Result<OidcMetadata> {
    let issuer = state.config.auth.oauth.issuer_url.trim_end_matches('/');
    let discovery = format!("{issuer}/.well-known/openid-configuration");
    let metadata = match state.http.get(discovery).send().await {
        Ok(response) if response.status().is_success() => {
            read_oauth_json(response, "oauth discovery metadata").await?
        }
        _ => OidcMetadata {
            authorization_endpoint: Some(format!("{issuer}/authorize")),
            token_endpoint: Some(format!("{issuer}/token")),
            userinfo_endpoint: Some(format!("{issuer}/userinfo")),
        },
    };
    validate_oauth_metadata_endpoints(&metadata)?;
    Ok(metadata)
}

async fn read_oauth_json<T: DeserializeOwned>(
    response: reqwest::Response,
    context: &str,
) -> anyhow::Result<T> {
    let bytes = read_oauth_json_body(response).await?;
    serde_json::from_slice(&bytes)
        .map_err(|error| anyhow::anyhow!("{context} is invalid JSON: {error}"))
}

async fn read_oauth_json_body(response: reqwest::Response) -> anyhow::Result<Vec<u8>> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        append_oauth_json_body_chunk(&mut body, &chunk?)?;
    }
    Ok(body)
}

fn append_oauth_json_body_chunk(body: &mut Vec<u8>, chunk: &[u8]) -> anyhow::Result<()> {
    if chunk.len() > OAUTH_JSON_BODY_LIMIT_BYTES.saturating_sub(body.len()) {
        anyhow::bail!(
            "oauth JSON response exceeds configured limit of {OAUTH_JSON_BODY_LIMIT_BYTES} bytes"
        );
    }
    body.extend_from_slice(chunk);
    Ok(())
}

fn validate_oauth_metadata_endpoints(metadata: &OidcMetadata) -> anyhow::Result<()> {
    for (field, endpoint) in [
        (
            "authorization_endpoint",
            metadata.authorization_endpoint.as_deref(),
        ),
        ("token_endpoint", metadata.token_endpoint.as_deref()),
        ("userinfo_endpoint", metadata.userinfo_endpoint.as_deref()),
    ] {
        let Some(endpoint) = endpoint else {
            continue;
        };
        let url = Url::parse(endpoint)
            .map_err(|error| anyhow::anyhow!("oauth {field} is not a valid URL: {error}"))?;
        if url.host_str().is_none() {
            anyhow::bail!("oauth {field} must include a host");
        }
        if !matches!(url.scheme(), "http" | "https") {
            anyhow::bail!("oauth {field} must use http or https");
        }
        if !url.username().is_empty() || url.password().is_some() {
            anyhow::bail!("oauth {field} must not contain credentials");
        }
    }
    Ok(())
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
    let row = auth_db_timeout("session lookup", async {
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
        Ok(row)
    })
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
    development_password
        .is_some_and(|expected| constant_time_eq(expected.as_bytes(), password.as_bytes()))
}

fn verify_local_admin_credentials(
    admin: &LocalAdminConfig,
    username: &str,
    password: &str,
) -> bool {
    // Always verify the password before combining results so unknown usernames
    // do not skip expensive hash verification.
    let password_valid = verify_password(
        password,
        admin.password_hash.as_deref(),
        admin.password.as_deref(),
    );
    let username_valid = constant_time_eq(admin.username.as_bytes(), username.as_bytes());
    password_valid & username_valid
}

async fn verify_local_admin_credentials_blocking(
    admin: LocalAdminConfig,
    username: String,
    password: String,
) -> anyhow::Result<bool> {
    tokio::task::spawn_blocking(move || {
        verify_local_admin_credentials(&admin, &username, &password)
    })
    .await
    .map_err(|error| anyhow::anyhow!("local admin credential verification task failed: {error}"))
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

async fn create_session(
    state: &AppState,
    user_id: &str,
    display_name: &str,
    login_method: LoginMethod,
) -> anyhow::Result<String> {
    validate_identity_field("session user_id", user_id)?;
    validate_identity_field("session display_name", display_name)?;
    let session_id = random_token();
    let expires_at = Utc::now() + Duration::hours(state.config.auth.session_ttl_hours);
    auth_db_timeout("session create", async {
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
        Ok(())
    })
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
    let user_id = user_id.map(|value| truncate_utf8(&value, MAX_AUDIT_TEXT_BYTES));
    let remote_addr = remote_addr.map(|value| truncate_utf8(&value, MAX_AUDIT_TEXT_BYTES));
    auth_db_timeout("audit insert", async {
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
    })
    .await?;
    Ok(())
}

async fn auth_db_timeout<T, F>(operation: &str, future: F) -> anyhow::Result<T>
where
    F: Future<Output = anyhow::Result<T>>,
{
    auth_db_timeout_with_duration(operation, future, AUTH_DB_TIMEOUT).await
}

async fn auth_db_timeout_with_duration<T, F>(
    operation: &str,
    future: F,
    timeout_duration: StdDuration,
) -> anyhow::Result<T>
where
    F: Future<Output = anyhow::Result<T>>,
{
    match tokio::time::timeout(timeout_duration, future).await {
        Ok(result) => result,
        Err(_) => anyhow::bail!(
            "{operation} timed out after {} ms",
            timeout_duration.as_millis()
        ),
    }
}

fn login_payload_size_error(payload: &LoginRequest) -> Option<&'static str> {
    if payload.username.len() > MAX_LOGIN_USERNAME_BYTES {
        return Some("username_too_long");
    }
    if payload.password.len() > MAX_LOGIN_PASSWORD_BYTES {
        return Some("password_too_long");
    }
    None
}

fn login_failure_detail(retry_after: Option<StdDuration>, invalid_payload: Option<&str>) -> Value {
    let mut detail = serde_json::Map::new();
    if let Some(reason) = invalid_payload {
        detail.insert("invalid_payload".to_string(), json!(reason));
    }
    if let Some(retry_after) = retry_after {
        detail.insert("throttled".to_string(), json!(true));
        detail.insert(
            "retry_after_secs".to_string(),
            json!(retry_after_secs(retry_after)),
        );
    }
    Value::Object(detail)
}

fn validate_identity_field(field: &str, value: &str) -> anyhow::Result<()> {
    if value.len() > MAX_SESSION_IDENTITY_BYTES {
        anyhow::bail!("{field} must be at most {MAX_SESSION_IDENTITY_BYTES} bytes");
    }
    Ok(())
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }

    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn session_cookie(headers: &HeaderMap) -> Option<String> {
    named_cookie(headers, SESSION_COOKIE).filter(|value| valid_auth_token(value))
}

fn oauth_state_cookie(headers: &HeaderMap) -> Option<String> {
    named_cookie(headers, OAUTH_STATE_COOKIE).filter(|value| valid_auth_token(value))
}

fn named_cookie(headers: &HeaderMap, cookie_name: &str) -> Option<String> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in cookie.split(';') {
        let Some((name, value)) = part.trim().split_once('=') else {
            continue;
        };
        if name == cookie_name {
            return Some(value.to_string());
        }
    }
    None
}

fn valid_auth_token(value: &str) -> bool {
    value.len() == AUTH_TOKEN_ENCODED_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
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
    append_set_cookie(headers, &cookie);
}

fn clear_session_cookie(headers: &mut HeaderMap, state: &AppState) {
    let mut cookie = format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    if state.config.auth.cookie_secure {
        cookie.push_str("; Secure");
    }
    append_set_cookie(headers, &cookie);
}

fn set_oauth_state_cookie(headers: &mut HeaderMap, state: &AppState, state_value: &str) {
    append_set_cookie(
        headers,
        &oauth_state_cookie_header(
            state_value,
            OAUTH_STATE_TTL_SECS,
            state.config.auth.cookie_secure,
        ),
    );
}

fn clear_oauth_state_cookie(headers: &mut HeaderMap, state: &AppState) {
    append_set_cookie(
        headers,
        &oauth_state_cookie_header("", 0, state.config.auth.cookie_secure),
    );
}

fn oauth_state_cookie_header(value: &str, max_age_secs: i64, secure: bool) -> String {
    let mut cookie = format!(
        "{OAUTH_STATE_COOKIE}={value}; Path=/api/auth/oauth; HttpOnly; SameSite=Lax; Max-Age={max_age_secs}"
    );
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

fn append_set_cookie(headers: &mut HeaderMap, cookie: &str) {
    headers.append(header::SET_COOKIE, HeaderValue::from_str(cookie).unwrap());
}

fn random_token() -> String {
    let mut bytes = [0_u8; AUTH_TOKEN_BYTES];
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

fn oauth_authorization_location(
    authorization_endpoint: &str,
    client_id: &str,
    redirect_url: &str,
    state_value: &str,
) -> anyhow::Result<String> {
    let mut url = Url::parse(authorization_endpoint)?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_url)
        .append_pair("scope", "openid email profile")
        .append_pair("state", state_value);
    Ok(url.to_string())
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
    use argon2::password_hash::{PasswordHasher, SaltString};

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
    fn local_admin_credentials_accept_valid_hash_credentials() {
        let password_hash = test_password_hash("correct-password");
        let admin = LocalAdminConfig {
            username: "admin".to_string(),
            password: None,
            password_hash: Some(password_hash),
        };

        assert!(verify_local_admin_credentials(
            &admin,
            "admin",
            "correct-password"
        ));
    }

    #[test]
    fn local_admin_credentials_reject_wrong_user_even_with_valid_password() {
        let password_hash = test_password_hash("correct-password");
        let admin = LocalAdminConfig {
            username: "admin".to_string(),
            password: None,
            password_hash: Some(password_hash),
        };

        assert!(!verify_local_admin_credentials(
            &admin,
            "other-admin",
            "correct-password"
        ));
    }

    #[test]
    fn local_admin_credentials_reject_wrong_password_for_valid_user() {
        let password_hash = test_password_hash("correct-password");
        let admin = LocalAdminConfig {
            username: "admin".to_string(),
            password: None,
            password_hash: Some(password_hash),
        };

        assert!(!verify_local_admin_credentials(
            &admin,
            "admin",
            "wrong-password"
        ));
    }

    #[test]
    fn local_admin_credentials_support_development_plaintext_password() {
        let admin = LocalAdminConfig {
            username: "admin".to_string(),
            password: Some("dev-password".to_string()),
            password_hash: None,
        };

        assert!(verify_local_admin_credentials(
            &admin,
            "admin",
            "dev-password"
        ));
        assert!(!verify_local_admin_credentials(
            &admin,
            "admin",
            "wrong-password"
        ));
    }

    #[tokio::test]
    async fn local_admin_credentials_blocking_matches_sync_verifier() {
        let admin = LocalAdminConfig {
            username: "admin".to_string(),
            password: None,
            password_hash: Some(test_password_hash("correct-password")),
        };

        assert!(
            verify_local_admin_credentials_blocking(
                admin.clone(),
                "admin".to_string(),
                "correct-password".to_string(),
            )
            .await
            .unwrap()
        );
        assert!(
            !verify_local_admin_credentials_blocking(
                admin,
                "admin".to_string(),
                "wrong-password".to_string(),
            )
            .await
            .unwrap()
        );
    }

    #[test]
    fn login_payload_size_error_rejects_oversized_fields() {
        let payload = LoginRequest {
            username: "a".repeat(MAX_LOGIN_USERNAME_BYTES + 1),
            password: "password".to_string(),
        };

        assert_eq!(
            login_payload_size_error(&payload),
            Some("username_too_long")
        );

        let payload = LoginRequest {
            username: "admin".to_string(),
            password: "a".repeat(MAX_LOGIN_PASSWORD_BYTES + 1),
        };

        assert_eq!(
            login_payload_size_error(&payload),
            Some("password_too_long")
        );
    }

    #[test]
    fn login_payload_size_error_accepts_boundary_lengths() {
        let payload = LoginRequest {
            username: "a".repeat(MAX_LOGIN_USERNAME_BYTES),
            password: "a".repeat(MAX_LOGIN_PASSWORD_BYTES),
        };

        assert_eq!(login_payload_size_error(&payload), None);
    }

    #[test]
    fn login_failure_detail_reports_invalid_payload_and_throttle() {
        let detail =
            login_failure_detail(Some(StdDuration::from_secs(12)), Some("password_too_long"));

        assert_eq!(detail["invalid_payload"], json!("password_too_long"));
        assert_eq!(detail["throttled"], json!(true));
        assert_eq!(detail["retry_after_secs"], json!(12));
    }

    #[test]
    fn identity_fields_are_bounded() {
        assert!(validate_identity_field("user", &"a".repeat(MAX_SESSION_IDENTITY_BYTES)).is_ok());

        let error = validate_identity_field("user", &"a".repeat(MAX_SESSION_IDENTITY_BYTES + 1))
            .unwrap_err()
            .to_string();

        assert!(error.contains("must be at most"));
    }

    #[test]
    fn truncate_utf8_respects_char_boundaries() {
        let value = format!("{}é", "a".repeat(MAX_AUDIT_TEXT_BYTES - 1));
        let truncated = truncate_utf8(&value, MAX_AUDIT_TEXT_BYTES);

        assert_eq!(truncated.len(), MAX_AUDIT_TEXT_BYTES - 1);
        assert!(truncated.is_char_boundary(truncated.len()));
    }

    #[tokio::test]
    async fn auth_db_timeout_allows_fast_future() {
        let value = auth_db_timeout_with_duration(
            "test op",
            async { Ok::<_, anyhow::Error>(42) },
            StdDuration::from_millis(1),
        )
        .await
        .unwrap();

        assert_eq!(value, 42);
    }

    #[tokio::test]
    async fn auth_db_timeout_rejects_slow_future() {
        let error = auth_db_timeout_with_duration(
            "test op",
            async { std::future::pending::<anyhow::Result<()>>().await },
            StdDuration::from_millis(1),
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(error.contains("test op timed out"));
    }

    #[test]
    fn oauth_metadata_endpoint_validation_accepts_https_endpoints() {
        let metadata = oauth_metadata_with_endpoints(
            "https://issuer.example.com/authorize",
            "https://issuer.example.com/token",
            "https://issuer.example.com/userinfo",
        );

        validate_oauth_metadata_endpoints(&metadata).unwrap();
    }

    #[test]
    fn oauth_metadata_endpoint_validation_accepts_http_endpoints() {
        let metadata = oauth_metadata_with_endpoints(
            "http://issuer.example.com/authorize",
            "http://issuer.example.com/token",
            "http://issuer.example.com/userinfo",
        );

        validate_oauth_metadata_endpoints(&metadata).unwrap();
    }

    #[test]
    fn oauth_metadata_endpoint_validation_rejects_non_http_or_credentialed_endpoints() {
        let metadata = oauth_metadata_with_endpoints(
            "javascript:alert(1)",
            "https://user:secret@issuer.example.com/token",
            "https://issuer.example.com/userinfo",
        );

        let error = validate_oauth_metadata_endpoints(&metadata)
            .unwrap_err()
            .to_string();

        assert!(error.contains("authorization_endpoint must include a host"));

        let metadata = oauth_metadata_with_endpoints(
            "https://issuer.example.com/authorize",
            "https://user:secret@issuer.example.com/token",
            "https://issuer.example.com/userinfo",
        );
        let error = validate_oauth_metadata_endpoints(&metadata)
            .unwrap_err()
            .to_string();

        assert!(error.contains("token_endpoint must not contain credentials"));
    }

    #[test]
    fn oauth_json_body_limit_allows_exact_limit_across_chunks() {
        let mut body = Vec::new();

        append_oauth_json_body_chunk(&mut body, &vec![b'a'; OAUTH_JSON_BODY_LIMIT_BYTES - 1])
            .unwrap();
        append_oauth_json_body_chunk(&mut body, b"b").unwrap();

        assert_eq!(body.len(), OAUTH_JSON_BODY_LIMIT_BYTES);
    }

    #[test]
    fn oauth_json_body_limit_rejects_oversized_response() {
        let mut body = vec![b'a'; OAUTH_JSON_BODY_LIMIT_BYTES];
        let error = append_oauth_json_body_chunk(&mut body, b"b")
            .unwrap_err()
            .to_string();

        assert!(error.contains("oauth JSON response exceeds configured limit"));
        assert_eq!(body.len(), OAUTH_JSON_BODY_LIMIT_BYTES);
    }

    #[test]
    fn oauth_authorization_location_preserves_query_and_encodes_parameters() {
        let location = oauth_authorization_location(
            "https://issuer.example.com/authorize?prompt=login",
            "client id",
            "https://llmtrace.example.com/api/auth/oauth/callback?next=/ui/",
            "state with space",
        )
        .unwrap();
        let url = Url::parse(&location).unwrap();
        let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();

        assert_eq!(
            url.origin().ascii_serialization(),
            "https://issuer.example.com"
        );
        assert_eq!(url.path(), "/authorize");
        assert!(pairs.contains(&("prompt".to_string(), "login".to_string())));
        assert!(pairs.contains(&("response_type".to_string(), "code".to_string())));
        assert!(pairs.contains(&("client_id".to_string(), "client id".to_string())));
        assert!(pairs.contains(&(
            "redirect_uri".to_string(),
            "https://llmtrace.example.com/api/auth/oauth/callback?next=/ui/".to_string(),
        )));
        assert!(pairs.contains(&("scope".to_string(), "openid email profile".to_string())));
        assert!(pairs.contains(&("state".to_string(), "state with space".to_string())));
    }

    #[test]
    fn oauth_authorization_location_rejects_invalid_endpoint() {
        let error = oauth_authorization_location(
            "/authorize",
            "client",
            "https://llmtrace.example.com/api/auth/oauth/callback",
            "state",
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("relative URL without a base"));
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

    #[test]
    fn session_cookie_reads_cookie_after_malformed_segments() {
        let token = test_auth_token('a');
        let cookie = format!("bad-cookie; theme=dark; llmtrace_session={token}");
        let headers = headers_with(header::COOKIE, &cookie);

        assert_eq!(session_cookie(&headers).as_deref(), Some(token.as_str()));
    }

    #[test]
    fn session_cookie_ignores_malformed_segments_without_session_cookie() {
        let headers = headers_with(header::COOKIE, "bad-cookie; theme=dark");

        assert_eq!(session_cookie(&headers), None);
    }

    #[test]
    fn session_cookie_ignores_malformed_token_values() {
        let headers = headers_with(
            header::COOKIE,
            "llmtrace_session=short-token; llmtrace_oauth_state=also-short",
        );

        assert_eq!(session_cookie(&headers), None);
        assert_eq!(oauth_state_cookie(&headers), None);
    }

    #[test]
    fn oauth_state_cookie_reads_only_oauth_state_cookie() {
        let session = test_auth_token('a');
        let state = test_auth_token('b');
        let cookie = format!("llmtrace_session={session}; llmtrace_oauth_state={state}");
        let headers = headers_with(header::COOKIE, &cookie);

        assert_eq!(
            oauth_state_cookie(&headers).as_deref(),
            Some(state.as_str())
        );
    }

    #[test]
    fn oauth_state_cookie_header_is_short_lived_and_path_scoped() {
        let cookie = oauth_state_cookie_header("state-456", OAUTH_STATE_TTL_SECS, true);

        assert!(cookie.starts_with("llmtrace_oauth_state=state-456;"));
        assert!(cookie.contains("Path=/api/auth/oauth"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Lax"));
        assert!(cookie.contains("Max-Age=600"));
        assert!(cookie.contains("Secure"));
    }

    #[test]
    fn append_set_cookie_preserves_multiple_cookie_headers() {
        let mut headers = HeaderMap::new();

        append_set_cookie(&mut headers, "llmtrace_session=session-123; Path=/");
        append_set_cookie(
            &mut headers,
            "llmtrace_oauth_state=; Path=/api/auth/oauth; Max-Age=0",
        );

        let values = headers
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|value| value.to_str().unwrap().to_string())
            .collect::<Vec<_>>();

        assert_eq!(
            values,
            vec![
                "llmtrace_session=session-123; Path=/".to_string(),
                "llmtrace_oauth_state=; Path=/api/auth/oauth; Max-Age=0".to_string()
            ]
        );
    }

    fn headers_with(name: axum::http::HeaderName, value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(name, HeaderValue::from_str(value).unwrap());
        headers
    }

    fn test_password_hash(password: &str) -> String {
        let salt = SaltString::encode_b64(b"llmtrace-test-salt").unwrap();
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .unwrap()
            .to_string()
    }

    fn test_auth_token(fill: char) -> String {
        fill.to_string().repeat(AUTH_TOKEN_ENCODED_LEN)
    }

    fn oauth_metadata_with_endpoints(
        authorization_endpoint: &str,
        token_endpoint: &str,
        userinfo_endpoint: &str,
    ) -> OidcMetadata {
        OidcMetadata {
            authorization_endpoint: Some(authorization_endpoint.to_string()),
            token_endpoint: Some(token_endpoint.to_string()),
            userinfo_endpoint: Some(userinfo_endpoint.to_string()),
        }
    }
}
