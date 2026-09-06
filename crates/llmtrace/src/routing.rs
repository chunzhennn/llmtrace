//! Forwarding and capture are independent: supporting gateway APIs need no archives.
use axum::http::Method;

use crate::config::{PathPrefixAllowlist, ProxyConfig, ProxyPreset};

const INFERENCE: &[&str] = &[
    "/chat/completions",
    "/completions",
    "/responses",
    "/messages",
    "/embeddings",
    "/images",
    "/audio",
    "/moderations",
    "/rerank",
    "/realtime",
];
const SUPPORTING: &[&str] = &[
    "/models",
    "/files",
    "/batches",
    "/assistants",
    "/threads",
    "/fine_tuning",
    "/model/info",
    "/key/info",
    "/user/info",
    "/health",
];

/// Routing and audit decisions must not change when the URL library or upstream
/// normalizes a path. Ordinary percent-encoded identifiers remain valid.
pub fn unambiguous_path(path: &str) -> bool {
    !path.contains('\\')
        && !path.split('/').any(|segment| {
            let mut dots = 0;
            let mut bytes = segment.as_bytes();
            while !bytes.is_empty() {
                if bytes[0] == b'.' {
                    dots += 1;
                    bytes = &bytes[1..];
                } else if bytes.len() >= 3
                    && bytes[0] == b'%'
                    && bytes[1] == b'2'
                    && bytes[2].eq_ignore_ascii_case(&b'e')
                {
                    dots += 1;
                    bytes = &bytes[3..];
                } else {
                    // Encoded path separators can be decoded before upstream routing.
                    return bytes
                        .windows(3)
                        .any(|s| s.eq_ignore_ascii_case(b"%2f") || s.eq_ignore_ascii_case(b"%5c"));
                }
            }
            dots == 1 || dots == 2
        })
}

pub fn litellm_capture_prefixes() -> Vec<String> {
    INFERENCE
        .iter()
        .flat_map(|path| [path.to_string(), format!("/v1{path}")])
        .collect()
}

pub fn litellm_forward_prefixes() -> Vec<String> {
    // Keep /v1 extensible as LiteLLM adds endpoints. Unversioned management
    // routes are intentionally excluded; deployments can explicitly opt in.
    std::iter::once("/v1".to_string())
        .chain(
            INFERENCE
                .iter()
                .chain(SUPPORTING)
                .map(|path| path.to_string()),
        )
        .collect()
}

#[derive(Clone)]
pub struct CapturePolicy {
    prefixes: PathPrefixAllowlist,
    inference_methods_only: bool,
}

impl CapturePolicy {
    pub fn new(config: &ProxyConfig) -> Result<Self, String> {
        Ok(Self {
            prefixes: config.capture_prefix_allowlist()?,
            inference_methods_only: config.preset == ProxyPreset::Litellm
                && config.capture_path_prefixes.is_none(),
        })
    }

    pub fn captures(&self, method: &Method, path: &str, websocket: bool) -> bool {
        self.prefixes.allows(path)
            && (!self.inference_methods_only || method == Method::POST || websocket)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ambiguous_paths_cannot_escape_forwarding_or_capture_prefixes() {
        for path in [
            "/v1/../key/generate",
            "/models/../chat/completions",
            "/v1/%2E%2e/key",
            "/v1/.%2e/key",
            "/v1/a%2fb",
            "/v1/a%5cb",
            "/v1/a\\b",
        ] {
            assert!(!unambiguous_path(path), "{path}");
        }
        for path in [
            "/v1/models",
            "/v1/files/id%20with%20space",
            "/v1/filename.json",
            "/v1/files/%E4%B8%AD",
        ] {
            assert!(unambiguous_path(path), "{path}");
        }
    }

    #[test]
    fn litellm_separates_forwarding_from_capture() {
        let config = ProxyConfig {
            preset: ProxyPreset::Litellm,
            ..Default::default()
        };
        let forward = config.path_prefix_allowlist().unwrap();
        let capture = CapturePolicy::new(&config).unwrap();
        for path in [
            "/chat/completions",
            "/v1/messages",
            "/responses",
            "/v1/embeddings",
            "/audio/transcriptions",
        ] {
            assert!(forward.allows(path), "{path}");
            assert!(capture.captures(&Method::POST, path, false), "{path}");
            assert!(!capture.captures(&Method::OPTIONS, path, false), "{path}");
        }
        assert!(capture.captures(&Method::GET, "/v1/realtime", true));
        for path in [
            "/models",
            "/v1/models",
            "/v1/files/1/content",
            "/batches/1",
            "/key/info",
            "/health/readiness",
            "/v1/future-endpoint",
        ] {
            assert!(forward.allows(path), "{path}");
            assert!(!capture.captures(&Method::POST, path, false), "{path}");
        }
        for path in [
            "/ui",
            "/api/requests",
            "/key/generate",
            "/key/info-leak",
            "/config",
            "/healthz",
            "/v10/messages",
            "/chat/completions-other",
        ] {
            assert!(!forward.allows(path), "{path}");
        }
    }

    #[test]
    fn custom_capture_overrides_do_not_expand_forwarding() {
        let config = ProxyConfig {
            preset: ProxyPreset::Litellm,
            path_prefixes: Some(vec!["/models".into()]),
            capture_path_prefixes: Some(vec!["/models".into(), "/private".into()]),
            ..Default::default()
        };
        assert!(
            CapturePolicy::new(&config)
                .unwrap()
                .captures(&Method::GET, "/models", false)
        );
        assert!(!config.path_prefix_allowlist().unwrap().allows("/private"));
        assert!(
            !config
                .path_prefix_allowlist()
                .unwrap()
                .allows("/v1/responses")
        );
    }

    #[test]
    fn legacy_capture_and_explicit_disable_are_preserved() {
        let mut config = ProxyConfig::default();
        assert!(
            CapturePolicy::new(&config)
                .unwrap()
                .captures(&Method::GET, "/v1/models", false)
        );
        config.capture_path_prefixes = Some(vec![]);
        assert!(!CapturePolicy::new(&config).unwrap().captures(
            &Method::POST,
            "/v1/chat/completions",
            false
        ));
    }
}
