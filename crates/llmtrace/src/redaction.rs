use std::borrow::Cow;

use axum::http::HeaderMap;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use url::Url;

use crate::config::RedactionConfig;
use crate::types::BodyRedaction;

const REDACTED_QUERY_VALUE: &str = "REDACTED";

#[derive(Debug, Clone)]
pub struct RedactedHeaders {
    pub json: Value,
    pub first_secret_hash: Option<String>,
}

pub fn redact_headers(headers: &HeaderMap, config: &RedactionConfig) -> RedactedHeaders {
    let mut map = Map::new();
    let mut first_secret_hash = None;

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
            if first_secret_hash.is_none() {
                first_secret_hash = value_hash.clone();
            }
            map.insert(name_text, redacted_value(&value_text, value_hash));
        } else if name_text == "x-llmtrace-upstream" {
            map.insert(name_text, redact_upstream_header(&value_text));
        } else {
            map.insert(name_text, json!(value_text));
        }
    }

    RedactedHeaders {
        json: Value::Object(map),
        first_secret_hash,
    }
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
        .filter(|scheme| scheme.len() <= 24);
    let prefix_len = value.len().min(6);
    let suffix_start = value.len().saturating_sub(4);
    json!({
        "redacted": true,
        "scheme": scheme,
        "sha256": hash,
        "prefix": &value[..prefix_len],
        "suffix": &value[suffix_start..],
        "length": value.len()
    })
}

/// Applies the configured redaction mode to a captured body before persistence.
pub fn redact_body(body: &[u8], mode: BodyRedaction) -> Cow<'_, [u8]> {
    match mode {
        BodyRedaction::Disabled => Cow::Borrowed(body),
        BodyRedaction::Drop => Cow::Borrowed(&[]),
        BodyRedaction::JsonSecrets => match redact_json_secrets(body) {
            Some(redacted) => Cow::Owned(redacted),
            None if body_looks_like_json(body) => Cow::Borrowed(&[]),
            None => Cow::Borrowed(body),
        },
    }
}

fn redact_json_secrets(body: &[u8]) -> Option<Vec<u8>> {
    let mut value: Value = serde_json::from_slice(body).ok()?;
    redact_json_value(&mut value);
    serde_json::to_vec(&value).ok()
}

fn body_looks_like_json(body: &[u8]) -> bool {
    body.iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| matches!(byte, b'{' | b'['))
}

fn redact_json_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, entry) in map.iter_mut() {
                if entry.is_string() && is_secret_key(key) {
                    *entry = json!("[redacted]");
                } else {
                    redact_json_value(entry);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_json_value),
        _ => {}
    }
}

fn is_secret_key(key: &str) -> bool {
    const SECRET_HINTS: &[&str] = &[
        "authorization",
        "api_key",
        "apikey",
        "api-key",
        "secret",
        "password",
        "token",
    ];
    let key = key.to_ascii_lowercase();
    SECRET_HINTS.iter().any(|hint| key.contains(hint))
}

fn redact_upstream_header(value: &str) -> Value {
    match Url::parse(value) {
        Ok(mut url) => {
            let had_secret = !url.username().is_empty() || url.password().is_some();
            let _ = url.set_username("");
            let _ = url.set_password(None);
            if had_secret {
                json!({
                    "redacted": true,
                    "url": url.to_string(),
                    "sha256": sha256_hex(value.as_bytes())
                })
            } else {
                json!(url.to_string())
            }
        }
        Err(_) => json!(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderValue, header};

    #[test]
    fn redact_body_drop_clears_payload() {
        assert!(redact_body(b"{\"a\":1}", BodyRedaction::Drop).is_empty());
    }

    #[test]
    fn redact_body_disabled_is_passthrough() {
        let body = b"{\"a\":1}";
        assert_eq!(redact_body(body, BodyRedaction::Disabled).as_ref(), body);
    }

    #[test]
    fn redact_body_json_secrets_masks_nested_secret_strings() {
        let body =
            br#"{"model":"x","headers":{"Authorization":"Bearer sk"},"api_key":"abc","n":1}"#;
        let redacted = redact_body(body, BodyRedaction::JsonSecrets);
        let value: Value = serde_json::from_slice(&redacted).unwrap();

        assert_eq!(value["model"], json!("x"));
        assert_eq!(value["n"], json!(1));
        assert_eq!(value["api_key"], json!("[redacted]"));
        assert_eq!(value["headers"]["Authorization"], json!("[redacted]"));
    }

    #[test]
    fn redact_body_json_secrets_passthrough_on_non_json() {
        let body = b"not json";
        assert_eq!(redact_body(body, BodyRedaction::JsonSecrets).as_ref(), body);
    }

    #[test]
    fn redact_body_json_secrets_drops_malformed_json_like_body() {
        let body = br#" {"api_key":"sk-example""#;

        assert!(redact_body(body, BodyRedaction::JsonSecrets).is_empty());
    }

    #[test]
    fn redact_body_json_secrets_drops_malformed_json_arrays() {
        let body = br#" [{"token":"secret"} "#;

        assert!(redact_body(body, BodyRedaction::JsonSecrets).is_empty());
    }

    #[test]
    fn redact_headers_masks_cookie_headers_by_default() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_static("session=secret"));
        headers.insert(
            header::SET_COOKIE,
            HeaderValue::from_static("upstream=secret; HttpOnly"),
        );

        let redacted = redact_headers(&headers, &RedactionConfig::default());

        assert_eq!(redacted.json["cookie"]["redacted"], json!(true));
        assert_eq!(redacted.json["set-cookie"]["redacted"], json!(true));
        assert!(redacted.first_secret_hash.is_some());
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
