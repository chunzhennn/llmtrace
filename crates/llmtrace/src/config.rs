use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::types::{BodyRedaction, PluginHook};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub proxy: ProxyConfig,
    pub storage: StorageConfig,
    pub auth: AuthConfig,
    pub redaction: RedactionConfig,
    pub plugins: Vec<PluginConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ServerConfig {
    pub listen: String,
    pub public_url: String,
    pub ui_enabled: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ProxyConfig {
    pub default_upstream: String,
    pub allow_upstreams: Vec<String>,
    pub upstream_header: String,
    pub timeout_secs: u64,
    pub max_body_capture_bytes: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct StorageConfig {
    pub postgres_url: String,
    pub max_connections: u32,
    pub trace_queue_capacity: usize,
    pub trace_worker_count: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AuthConfig {
    pub cookie_secure: bool,
    pub session_ttl_hours: i64,
    pub local_admin: LocalAdminConfig,
    pub oauth: OAuthConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct LocalAdminConfig {
    pub username: String,
    pub password: Option<String>,
    pub password_hash: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct OAuthConfig {
    pub enabled: bool,
    pub issuer_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_url: String,
    pub allowed_emails: Vec<String>,
    pub allowed_domains: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct RedactionConfig {
    pub sensitive_headers: Vec<String>,
    pub store_header_hash: bool,
    pub body_redaction: BodyRedaction,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct PluginConfig {
    pub name: String,
    pub wasm_path: PathBuf,
    pub hooks: Vec<PluginHook>,
    pub timeout_ms: u64,
}

impl Config {
    pub fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        let mut config = if let Some(path) = path {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read config {}", path.display()))?;
            toml::from_str::<Config>(&text)
                .with_context(|| format!("failed to parse config {}", path.display()))?
        } else if Path::new("llmtrace.toml").exists() {
            let text =
                std::fs::read_to_string("llmtrace.toml").context("failed to read llmtrace.toml")?;
            toml::from_str::<Config>(&text).context("failed to parse llmtrace.toml")?
        } else {
            Config::default()
        };

        if let Ok(value) = std::env::var("DATABASE_URL") {
            config.storage.postgres_url = value;
        }
        if let Ok(value) = std::env::var("LLMTRACE_LISTEN") {
            config.server.listen = value;
        }
        if let Ok(value) = std::env::var("LLMTRACE_DEFAULT_UPSTREAM") {
            config.proxy.default_upstream = value;
        }
        if let Ok(value) = std::env::var("LLMTRACE_ADMIN_USERNAME") {
            config.auth.local_admin.username = value;
        }
        if let Ok(value) = std::env::var("LLMTRACE_ADMIN_PASSWORD") {
            config.auth.local_admin.password = Some(value);
        }
        if let Ok(value) = std::env::var("LLMTRACE_ADMIN_PASSWORD_HASH") {
            config.auth.local_admin.password_hash = Some(value);
        }

        Ok(config)
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:3000".to_string(),
            public_url: "http://127.0.0.1:3000".to_string(),
            ui_enabled: true,
        }
    }
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            default_upstream: "https://api.openai.com".to_string(),
            allow_upstreams: Vec::new(),
            upstream_header: "x-llmtrace-upstream".to_string(),
            timeout_secs: 300,
            max_body_capture_bytes: 1024 * 1024,
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            postgres_url: "postgres://postgres:postgres@localhost:5432/llmtrace".to_string(),
            max_connections: 10,
            trace_queue_capacity: 4096,
            trace_worker_count: 4,
        }
    }
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            cookie_secure: false,
            session_ttl_hours: 24,
            local_admin: LocalAdminConfig::default(),
            oauth: OAuthConfig::default(),
        }
    }
}

impl Default for LocalAdminConfig {
    fn default() -> Self {
        Self {
            username: "admin".to_string(),
            password: Some("admin".to_string()),
            password_hash: None,
        }
    }
}

impl Default for RedactionConfig {
    fn default() -> Self {
        Self {
            sensitive_headers: vec![
                "authorization".to_string(),
                "proxy-authorization".to_string(),
                "api-key".to_string(),
                "x-api-key".to_string(),
                "openai-api-key".to_string(),
                "anthropic-api-key".to_string(),
                "x-goog-api-key".to_string(),
            ],
            store_header_hash: true,
            body_redaction: BodyRedaction::Disabled,
        }
    }
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            wasm_path: PathBuf::new(),
            hooks: Vec::new(),
            timeout_ms: 50,
        }
    }
}
