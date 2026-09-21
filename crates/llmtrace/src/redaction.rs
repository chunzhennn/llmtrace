use axum::http::HeaderMap;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use url::Url;

use crate::config::RedactionConfig;

const REDACTED_QUERY_VALUE: &str = "REDACTED";

#[derive(Debug, Clone)]
pub struct RedactedHeaders {
    pub json: Value,
    pub first_secret_hash: Option<String>,
    /// Internal grouping identity; independent of persisted header fingerprints.
    pub credential_scope_hash: Option<String>,
}

pub fn redact_headers(
    headers: &HeaderMap,
    config: &RedactionConfig,
    upstream_header: &str,
) -> RedactedHeaders {
    let mut map = Map::new();
    // Account attribution must not depend on HeaderMap iteration order or on
    // unrelated cookies, CSRF tokens, or passwords.
    let credential_scope_hash = CREDENTIAL_HEADERS
        .iter()
        .find_map(|name| headers.get(*name))
        .map(|value| sha256_hex(value.as_bytes()));
    let first_secret_hash = config
        .store_header_hash
        .then(|| credential_scope_hash.clone())
        .flatten();
    let upstream_header = upstream_header.trim().to_ascii_lowercase();

    for (name, value) in headers {
        let name_text = name.as_str().to_ascii_lowercase();
        let value_text = value
            .to_str()
            .map(str::to_string)
            .unwrap_or_else(|_| "<non-utf8>".to_string());

        if is_sensitive_header(&name_text, config) {
            let value_hash = config
                .store_header_hash
                .then(|| sha256_hex(value_text.as_bytes()));
            map.insert(name_text, redacted_value(&value_text, value_hash));
        } else if name_text == upstream_header {
            map.insert(name_text, redact_upstream_header(&value_text));
        } else {
            map.insert(name_text, json!(value_text));
        }
    }

    RedactedHeaders {
        json: Value::Object(map),
        first_secret_hash,
        credential_scope_hash,
    }
}

const CREDENTIAL_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    "api-key",
    "openai-api-key",
    "anthropic-api-key",
    "x-goog-api-key",
];

/// Older journal records have original plugin headers but no explicit scope field.
pub fn credential_scope_from_headers(headers: &Value) -> Option<String> {
    CREDENTIAL_HEADERS
        .iter()
        .find_map(|name| headers.get(*name).and_then(Value::as_str))
        .filter(|value| *value != "<non-utf8>")
        .map(|value| sha256_hex(value.as_bytes()))
}

pub fn headers_to_json(headers: &HeaderMap) -> Value {
    let mut map = Map::new();
    for (name, value) in headers {
        let value_text = value
            .to_str()
            .map(str::to_string)
            .unwrap_or_else(|_| "<non-utf8>".to_string());
        map.insert(name.as_str().to_ascii_lowercase(), json!(value_text));
    }
    Value::Object(map)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Redacts all URI query parameter values before data leaves the trace pipeline.
pub fn redact_uri_query_values(value: &str) -> String {
    if let Ok(mut url) = Url::parse(value) {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        if let Some(query) = url.query() {
            let redacted_query = redact_query_string(query);
            url.set_query(Some(&redacted_query));
        }
        url.set_fragment(None);
        return url.to_string();
    }

    let without_fragment = value.split_once('#').map_or(value, |(prefix, _)| prefix);
    let Some((path, query)) = without_fragment.split_once('?') else {
        return without_fragment.to_string();
    };
    format!("{path}?{}", redact_query_string(query))
}

fn is_sensitive_header(name: &str, config: &RedactionConfig) -> bool {
    config
        .sensitive_headers
        .iter()
        .any(|candidate| header_name_matches(name, &candidate.to_ascii_lowercase()))
        || name.contains("api-key")
        || name.contains("apikey")
        || name.contains("token")
        || name.contains("secret")
        || name.contains("password")
        || name == "authorization"
        || name == "proxy-authorization"
        || name == "cookie"
        || name == "set-cookie"
}

fn header_name_matches(name: &str, candidate: &str) -> bool {
    name == candidate
        || (candidate.ends_with('*') && name.starts_with(candidate.trim_end_matches('*')))
}

fn redact_query_string(query: &str) -> String {
    if query.is_empty() {
        return String::new();
    }

    query
        .split('&')
        .map(|pair| {
            let name = pair.split_once('=').map_or(pair, |(name, _)| name);
            format!("{name}={REDACTED_QUERY_VALUE}")
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn redacted_value(value: &str, hash: Option<String>) -> Value {
    let scheme = value
        .split_once(' ')
        .map(|(scheme, _)| scheme.to_string())
        .filter(|scheme| {
            scheme.eq_ignore_ascii_case("bearer") || scheme.eq_ignore_ascii_case("basic")
        });
    json!({
        "redacted": true,
        "scheme": scheme,
        "sha256": hash,
        "length": value.len()
    })
}

fn redact_upstream_header(value: &str) -> Value {
    match Url::parse(value) {
        Ok(url) => {
            let had_secret = !url.username().is_empty() || url.password().is_some();
            let redacted_url = redact_uri_query_values(value);
            if had_secret {
                json!({
                    "redacted": true,
                    "url": redacted_url,
                    "sha256": sha256_hex(value.as_bytes())
                })
            } else {
                json!(redacted_url)
            }
        }
        Err(_) => json!(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacted_values_do_not_reveal_short_or_unicode_secrets() {
        for secret in ["short", "é界🔑"] {
            let value = redacted_value(secret, None);
            assert!(value.get("prefix").is_none());
            assert!(value.get("suffix").is_none());
        }
    }

    #[test]
    fn credential_hash_is_stable_when_cookies_change() {
        let mut headers = HeaderMap::new();
        headers.insert("cookie", "changing-cookie".parse().unwrap());
        headers.insert("authorization", "Bearer key".parse().unwrap());
        let redacted = redact_headers(&headers, &RedactionConfig::default(), "x-upstream");
        assert_eq!(redacted.first_secret_hash, Some(sha256_hex(b"Bearer key")));
        headers.remove("authorization");
        assert_eq!(
            redact_headers(&headers, &RedactionConfig::default(), "x-upstream").first_secret_hash,
            None
        );
    }
    use axum::http::{HeaderValue, header};

    #[test]
    fn redact_headers_masks_cookie_headers_by_default() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_static("session=secret"));
        headers.insert(
            header::SET_COOKIE,
            HeaderValue::from_static("upstream=secret; HttpOnly"),
        );

        let redacted = redact_headers(&headers, &RedactionConfig::default(), "x-llmtrace-upstream");

        assert_eq!(redacted.json["cookie"]["redacted"], json!(true));
        assert_eq!(redacted.json["set-cookie"]["redacted"], json!(true));
        assert!(redacted.first_secret_hash.is_none());
    }

    #[test]
    fn redact_headers_masks_token_secret_and_password_headers_by_default() {
        let mut headers = HeaderMap::new();
        headers.insert("x-auth-token", HeaderValue::from_static("token-secret"));
        headers.insert("x-client-secret", HeaderValue::from_static("client-secret"));
        headers.insert("x-password", HeaderValue::from_static("password-secret"));

        let redacted = redact_headers(&headers, &RedactionConfig::default(), "x-llmtrace-upstream");

        assert_eq!(redacted.json["x-auth-token"]["redacted"], json!(true));
        assert_eq!(redacted.json["x-client-secret"]["redacted"], json!(true));
        assert_eq!(redacted.json["x-password"]["redacted"], json!(true));
    }

    #[test]
    fn redact_headers_redacts_configured_upstream_header_url_query_values() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-target-upstream",
            HeaderValue::from_static(
                "https://api.example.com/v1/messages?api_key=sk-secret&debug=true",
            ),
        );

        let redacted = redact_headers(&headers, &RedactionConfig::default(), "x-target-upstream");

        assert_eq!(
            redacted.json["x-target-upstream"],
            json!("https://api.example.com/v1/messages?api_key=REDACTED&debug=REDACTED")
        );
    }

    #[test]
    fn redact_headers_hashes_credentialed_upstream_header_url() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-target-upstream",
            HeaderValue::from_static("https://user:secret@api.example.com/v1?token=secret"),
        );

        let redacted = redact_headers(&headers, &RedactionConfig::default(), "x-target-upstream");

        assert_eq!(redacted.json["x-target-upstream"]["redacted"], json!(true));
        assert_eq!(
            redacted.json["x-target-upstream"]["url"],
            json!("https://api.example.com/v1?token=REDACTED")
        );
        assert!(redacted.json["x-target-upstream"]["sha256"].is_string());
    }

    #[test]
    fn redact_uri_query_values_redacts_relative_uri_values() {
        assert_eq!(
            redact_uri_query_values("/v1/messages?api_key=sk-secret&debug=true"),
            "/v1/messages?api_key=REDACTED&debug=REDACTED"
        );
    }

    #[test]
    fn redact_uri_query_values_redacts_absolute_url_values_credentials_and_fragment() {
        assert_eq!(
            redact_uri_query_values(
                "https://user:secret@api.example.com/v1/messages?access_token=secret&debug=true#fragment"
            ),
            "https://api.example.com/v1/messages?access_token=REDACTED&debug=REDACTED"
        );
    }

    #[test]
    fn redact_uri_query_values_preserves_uris_without_query() {
        assert_eq!(
            redact_uri_query_values("/v1/messages"),
            "/v1/messages".to_string()
        );
        assert_eq!(
            redact_uri_query_values("https://api.example.com/v1/messages"),
            "https://api.example.com/v1/messages".to_string()
        );
    }

    #[test]
    fn redact_uri_query_values_preserves_empty_query() {
        assert_eq!(redact_uri_query_values("/v1/messages?"), "/v1/messages?");
    }
}
