use std::collections::HashSet;
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::Context;
use argon2::password_hash::PasswordHash;
use http::HeaderName;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::types::PluginHook;

const DEFAULT_LOCAL_ADMIN_PASSWORD: &str = "admin";
const MAX_SESSION_TTL_HOURS: i64 = 24 * 30;
const MAX_RETENTION_DAYS: i64 = 36500;
const MAX_RETENTION_PRUNE_BATCH_SIZE: i64 = 100_000;
const MAX_PROXY_TIMEOUT_SECS: u64 = 60 * 60;
const MAX_RESPONSE_BODY_BYTES: usize = 1024 * 1024 * 1024;
const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024 * 1024;
const MAX_WEBSOCKET_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_WEBSOCKET_SESSION_BYTES: usize = 2 * 1024 * 1024 * 1024;
const MAX_ARCHIVE_SEGMENT_UNCOMPRESSED_BYTES: usize = 2 * 1024 * 1024 * 1024;
const MAX_ARCHIVE_COMPRESSION_LEVEL: i32 = 10;
const MAX_STORAGE_CONNECTIONS: u32 = 1024;
const MAX_STORAGE_ACQUIRE_TIMEOUT_SECS: u64 = 300;
const MAX_TRACE_QUEUE_CAPACITY: usize = 1_000_000;
const MAX_TRACE_WORKER_COUNT: usize = 128;
const MAX_RETENTION_PRUNE_INTERVAL_SECS: u64 = 24 * 60 * 60;
const MAX_LOGIN_RATE_LIMIT_FAILURES: u32 = 1000;
const MAX_LOGIN_RATE_LIMIT_WINDOW_SECS: u64 = 24 * 60 * 60;
const MAX_LOGIN_RATE_LIMIT_LOCKOUT_SECS: u64 = 24 * 60 * 60;
const MAX_LOGIN_RATE_LIMIT_TRACKED_ENTRIES: usize = 1_000_000;
const MAX_OAUTH_TIMEOUT_SECS: u64 = 300;
const MAX_PLUGINS: usize = 64;
const MAX_PLUGIN_NAME_BYTES: usize = 128;
const MAX_PLUGIN_TIMEOUT_MS: u64 = 30_000;
const UPSTREAM_ALLOWLIST_SCHEMES: &[&str] = &["http", "https", "ws", "wss"];

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub proxy: ProxyConfig,
    pub storage: StorageConfig,
    pub archive: ArchiveConfig,
    pub auth: AuthConfig,
    pub observability: ObservabilityConfig,
    pub redaction: RedactionConfig,
    pub plugins: Vec<PluginConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ServerConfig {
    pub listen: String,
    pub public_url: String,
    pub deployment: DeploymentMode,
    pub ui_enabled: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ProxyConfig {
    pub default_upstream: String,
    pub allow_upstreams: Vec<String>,
    pub path_prefixes: Vec<String>,
    pub upstream_header: String,
    pub timeout_secs: u64,
    pub max_request_body_bytes: usize,
    pub max_response_body_bytes: usize,
    pub max_websocket_message_bytes: usize,
    pub max_websocket_session_bytes: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct StorageConfig {
    pub postgres_url: String,
    pub max_connections: u32,
    pub acquire_timeout_secs: u64,
    pub trace_queue_capacity: usize,
    pub trace_worker_count: usize,
    pub retention_days: Option<i64>,
    pub retention_prune_interval_secs: u64,
    pub retention_prune_batch_size: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ArchiveConfig {
    pub storage_backend: ArchiveStorageBackend,
    pub filesystem_root: PathBuf,
    pub segment_uncompressed_bytes: usize,
    pub compression_level: i32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AuthConfig {
    pub cookie_secure: bool,
    pub session_ttl_hours: i64,
    pub login_rate_limit: LoginRateLimitConfig,
    pub local_admin: LocalAdminConfig,
    pub oauth: OAuthConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct LoginRateLimitConfig {
    pub enabled: bool,
    pub max_failures: u32,
    pub window_secs: u64,
    pub lockout_secs: u64,
    pub max_tracked_entries: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct LocalAdminConfig {
    pub username: String,
    pub password: Option<String>,
    pub password_hash: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct OAuthConfig {
    pub enabled: bool,
    pub issuer_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_url: String,
    pub timeout_secs: u64,
    pub require_email_verified: bool,
    pub allowed_emails: Vec<String>,
    pub allowed_domains: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct ObservabilityConfig {
    pub metrics_bearer_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct RedactionConfig {
    pub sensitive_headers: Vec<String>,
    pub store_header_hash: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct PluginConfig {
    pub name: String,
    pub wasm_path: PathBuf,
    pub hooks: Vec<PluginHook>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UpstreamAllowEntry {
    Host {
        host: String,
        port: Option<u16>,
    },
    Url {
        scheme: String,
        host: String,
        port: u16,
        path_prefix: String,
    },
}

#[derive(Debug, Clone)]
pub struct UpstreamAllowlist {
    entries: Vec<UpstreamAllowEntry>,
}

#[derive(Debug, Clone, Default)]
pub struct PathPrefixAllowlist {
    prefixes: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentMode {
    #[default]
    Development,
    Production,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveStorageBackend {
    #[default]
    Filesystem,
    Postgres,
}

impl ArchiveStorageBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Filesystem => "filesystem",
            Self::Postgres => "postgres",
        }
    }
}

impl FromStr for ArchiveStorageBackend {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "filesystem" | "fs" => Ok(Self::Filesystem),
            "postgres" | "pg" => Ok(Self::Postgres),
            other => anyhow::bail!(
                "archive.storage_backend must be one of filesystem or postgres, got {other:?}"
            ),
        }
    }
}

impl fmt::Display for ArchiveStorageBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl DeploymentMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::Production => "production",
        }
    }

    fn is_production(self) -> bool {
        matches!(self, Self::Production)
    }
}

impl FromStr for DeploymentMode {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "development" | "dev" => Ok(Self::Development),
            "production" | "prod" => Ok(Self::Production),
            other => {
                anyhow::bail!("deployment must be one of development or production, got {other:?}")
            }
        }
    }
}

impl fmt::Display for DeploymentMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl ProxyConfig {
    pub fn upstream_allowlist(&self) -> Result<UpstreamAllowlist, String> {
        UpstreamAllowlist::parse(&self.allow_upstreams)
    }

    pub fn path_prefix_allowlist(&self) -> Result<PathPrefixAllowlist, String> {
        PathPrefixAllowlist::parse(&self.path_prefixes)
    }
}

impl UpstreamAllowlist {
    fn parse(entries: &[String]) -> Result<Self, String> {
        let entries = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                parse_upstream_allow_entry(&format!("proxy.allow_upstreams[{index}]"), entry)
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self { entries })
    }

    pub fn allows(&self, upstream_url: &Url) -> bool {
        if !UPSTREAM_ALLOWLIST_SCHEMES.contains(&upstream_url.scheme()) {
            return false;
        }
        if self.entries.is_empty() {
            return true;
        }

        self.entries.iter().any(|entry| entry.matches(upstream_url))
    }
}

impl PathPrefixAllowlist {
    fn parse(entries: &[String]) -> Result<Self, String> {
        let prefixes = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                parse_proxy_path_prefix(&format!("proxy.path_prefixes[{index}]"), entry)
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self { prefixes })
    }

    pub fn allows(&self, path: &str) -> bool {
        self.prefixes
            .iter()
            .any(|prefix| path_prefix_matches(prefix, path))
    }
}

impl UpstreamAllowEntry {
    fn matches(&self, upstream_url: &Url) -> bool {
        match self {
            Self::Host { host, port } => {
                upstream_url
                    .host_str()
                    .is_some_and(|upstream_host| upstream_host.eq_ignore_ascii_case(host))
                    && port.is_none_or(|port| upstream_url.port_or_known_default() == Some(port))
            }
            Self::Url {
                scheme,
                host,
                port,
                path_prefix,
            } => {
                upstream_url.scheme() == scheme
                    && upstream_url
                        .host_str()
                        .is_some_and(|upstream_host| upstream_host.eq_ignore_ascii_case(host))
                    && upstream_url.port_or_known_default() == Some(*port)
                    && path_prefix_matches(path_prefix, upstream_url.path())
            }
        }
    }
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

        apply_env_overrides(&mut config, |name| std::env::var(name).ok())?;
        config.normalize_sensitive_defaults();

        Ok(config)
    }

    pub fn log_startup_warnings(&self) {
        if !self.server.deployment.is_production() && self.proxy.path_prefixes.is_empty() {
            tracing::warn!("proxy.path_prefixes is empty; non-platform paths will not be proxied");
        }
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        let mut errors = Vec::new();

        self.validate_server(&mut errors);
        self.validate_proxy(&mut errors);
        self.validate_storage(&mut errors);
        self.validate_archive(&mut errors);
        self.validate_auth(&mut errors);
        self.validate_observability(&mut errors);
        self.validate_redaction(&mut errors);
        self.validate_plugins(&mut errors);

        if errors.is_empty() {
            Ok(())
        } else {
            anyhow::bail!("invalid config:\n- {}", errors.join("\n- "))
        }
    }

    fn normalize_sensitive_defaults(&mut self) {
        if self
            .auth
            .local_admin
            .password_hash
            .as_deref()
            .is_some_and(|hash| !hash.trim().is_empty())
            && self.auth.local_admin.password.as_deref() == Some(DEFAULT_LOCAL_ADMIN_PASSWORD)
        {
            self.auth.local_admin.password = None;
        }
    }

    fn validate_server(&self, errors: &mut Vec<String>) {
        if self.server.listen.trim().parse::<SocketAddr>().is_err() {
            errors.push(format!(
                "server.listen must be a socket address, got {:?}",
                self.server.listen
            ));
        }

        match parse_url(
            "server.public_url",
            &self.server.public_url,
            &["http", "https"],
        ) {
            Ok(_) => {}
            Err(error) => errors.push(error),
        }
    }

    fn validate_proxy(&self, errors: &mut Vec<String>) {
        match parse_url(
            "proxy.default_upstream",
            &self.proxy.default_upstream,
            &["http", "https"],
        ) {
            Ok(default_upstream) => {
                if !default_upstream.username().is_empty() || default_upstream.password().is_some()
                {
                    errors.push("proxy.default_upstream must not contain credentials".to_string());
                }
            }
            Err(error) => errors.push(error),
        }

        if self.proxy.upstream_header.trim().is_empty()
            || HeaderName::from_bytes(self.proxy.upstream_header.trim().as_bytes()).is_err()
        {
            errors.push(format!(
                "proxy.upstream_header must be a valid HTTP header name, got {:?}",
                self.proxy.upstream_header
            ));
        }

        validate_u64_range(
            errors,
            "proxy.timeout_secs",
            self.proxy.timeout_secs,
            MAX_PROXY_TIMEOUT_SECS,
        );
        validate_usize_range(
            errors,
            "proxy.max_request_body_bytes",
            self.proxy.max_request_body_bytes,
            MAX_REQUEST_BODY_BYTES,
        );
        validate_usize_range(
            errors,
            "proxy.max_response_body_bytes",
            self.proxy.max_response_body_bytes,
            MAX_RESPONSE_BODY_BYTES,
        );
        validate_usize_range(
            errors,
            "proxy.max_websocket_message_bytes",
            self.proxy.max_websocket_message_bytes,
            MAX_WEBSOCKET_MESSAGE_BYTES,
        );
        validate_usize_range(
            errors,
            "proxy.max_websocket_session_bytes",
            self.proxy.max_websocket_session_bytes,
            MAX_WEBSOCKET_SESSION_BYTES,
        );

        if self.server.deployment.is_production() && self.proxy.allow_upstreams.is_empty() {
            errors.push(
                "proxy.allow_upstreams must not be empty when server.deployment is production"
                    .to_string(),
            );
        }

        if self.server.deployment.is_production() && self.proxy.path_prefixes.is_empty() {
            errors.push(
                "proxy.path_prefixes must not be empty when server.deployment is production"
                    .to_string(),
            );
        }

        for (index, prefix) in self.proxy.path_prefixes.iter().enumerate() {
            if let Err(error) =
                parse_proxy_path_prefix(&format!("proxy.path_prefixes[{index}]"), prefix)
            {
                errors.push(error);
            }
        }

        for (index, upstream) in self.proxy.allow_upstreams.iter().enumerate() {
            if let Err(error) =
                parse_upstream_allow_entry(&format!("proxy.allow_upstreams[{index}]"), upstream)
            {
                errors.push(error);
            }
        }
    }

    fn validate_storage(&self, errors: &mut Vec<String>) {
        if let Err(error) = parse_url(
            "storage.postgres_url",
            &self.storage.postgres_url,
            &["postgres", "postgresql"],
        ) {
            errors.push(error);
        }
        validate_u32_range(
            errors,
            "storage.max_connections",
            self.storage.max_connections,
            MAX_STORAGE_CONNECTIONS,
        );
        validate_u64_range(
            errors,
            "storage.acquire_timeout_secs",
            self.storage.acquire_timeout_secs,
            MAX_STORAGE_ACQUIRE_TIMEOUT_SECS,
        );
        validate_usize_range(
            errors,
            "storage.trace_queue_capacity",
            self.storage.trace_queue_capacity,
            MAX_TRACE_QUEUE_CAPACITY,
        );
        validate_usize_range(
            errors,
            "storage.trace_worker_count",
            self.storage.trace_worker_count,
            MAX_TRACE_WORKER_COUNT,
        );
        match self.storage.retention_days {
            Some(days) if days <= 0 => {
                errors.push("storage.retention_days must be greater than 0 when set".to_string());
            }
            Some(days) if days > MAX_RETENTION_DAYS => {
                errors.push(format!(
                    "storage.retention_days must be at most {MAX_RETENTION_DAYS}"
                ));
            }
            Some(_) => {}
            None if self.server.deployment.is_production() => {
                errors.push(
                    "storage.retention_days is required when server.deployment is production"
                        .to_string(),
                );
            }
            None => {}
        }
        validate_u64_range(
            errors,
            "storage.retention_prune_interval_secs",
            self.storage.retention_prune_interval_secs,
            MAX_RETENTION_PRUNE_INTERVAL_SECS,
        );
        if self.storage.retention_prune_batch_size <= 0 {
            errors.push("storage.retention_prune_batch_size must be greater than 0".to_string());
        } else if self.storage.retention_prune_batch_size > MAX_RETENTION_PRUNE_BATCH_SIZE {
            errors.push(format!(
                "storage.retention_prune_batch_size must be at most {MAX_RETENTION_PRUNE_BATCH_SIZE}"
            ));
        }
    }

    fn validate_archive(&self, errors: &mut Vec<String>) {
        if self.archive.storage_backend == ArchiveStorageBackend::Filesystem
            && self.archive.filesystem_root.as_os_str().is_empty()
        {
            errors.push(
                "archive.filesystem_root must not be empty when archive.storage_backend is filesystem"
                    .to_string(),
            );
        }
        validate_usize_range(
            errors,
            "archive.segment_uncompressed_bytes",
            self.archive.segment_uncompressed_bytes,
            MAX_ARCHIVE_SEGMENT_UNCOMPRESSED_BYTES,
        );
        if self.archive.compression_level <= 0 {
            errors.push("archive.compression_level must be greater than 0".to_string());
        } else if self.archive.compression_level > MAX_ARCHIVE_COMPRESSION_LEVEL {
            errors.push(format!(
                "archive.compression_level must be at most {MAX_ARCHIVE_COMPRESSION_LEVEL}"
            ));
        }
    }

    fn validate_auth(&self, errors: &mut Vec<String>) {
        let local_admin = &self.auth.local_admin;
        if local_admin.username.trim().is_empty() {
            errors.push("auth.local_admin.username must not be empty".to_string());
        }

        let has_password = match local_admin.password.as_deref().map(str::trim) {
            Some("") => {
                errors.push("auth.local_admin.password must not be empty when set".to_string());
                false
            }
            Some(_) => true,
            None => false,
        };
        let has_hash = match local_admin.password_hash.as_deref().map(str::trim) {
            Some("") => {
                errors
                    .push("auth.local_admin.password_hash must not be empty when set".to_string());
                false
            }
            Some(hash) => {
                if PasswordHash::new(hash).is_err() {
                    errors.push(
                        "auth.local_admin.password_hash must be a valid PHC password hash"
                            .to_string(),
                    );
                    false
                } else {
                    true
                }
            }
            None => false,
        };

        if !has_password && !has_hash && !self.auth.oauth.enabled {
            errors.push(
                "configure auth.local_admin.password_hash, auth.local_admin.password, or enable auth.oauth"
                    .to_string(),
            );
        }

        if self.auth.session_ttl_hours <= 0 {
            errors.push("auth.session_ttl_hours must be greater than 0".to_string());
        } else if self.auth.session_ttl_hours > MAX_SESSION_TTL_HOURS {
            errors.push(format!(
                "auth.session_ttl_hours must be at most {MAX_SESSION_TTL_HOURS}"
            ));
        }

        self.validate_login_rate_limit(errors);

        if self.server.deployment.is_production() {
            if Url::parse(&self.server.public_url).is_ok_and(|url| url.scheme() == "https")
                && !self.auth.cookie_secure
            {
                errors.push(
                    "auth.cookie_secure must be true when server.public_url uses https in production"
                        .to_string(),
                );
            }
            if has_password {
                errors.push(
                    "auth.local_admin.password must not be used when server.deployment is production; set auth.local_admin.password_hash instead"
                        .to_string(),
                );
            }
            if !has_hash {
                errors.push(
                    "auth.local_admin.password_hash is required when server.deployment is production"
                        .to_string(),
                );
            }
            if !self.auth.login_rate_limit.enabled {
                errors.push(
                    "auth.login_rate_limit.enabled must be true when server.deployment is production"
                        .to_string(),
                );
            }
        }

        self.validate_oauth(errors);
    }

    fn validate_observability(&self, errors: &mut Vec<String>) {
        match self.observability.metrics_bearer_token.as_deref() {
            Some(token) if token.trim().is_empty() => errors
                .push("observability.metrics_bearer_token must not be empty when set".to_string()),
            Some(token)
                if !token.is_ascii()
                    || token
                        .bytes()
                        .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace()) =>
            {
                errors.push(
                    "observability.metrics_bearer_token must contain only printable non-whitespace ASCII"
                        .to_string(),
                );
            }
            Some(_) => {}
            None if self.server.deployment.is_production() => {
                errors.push(
                    "observability.metrics_bearer_token is required when server.deployment is production"
                        .to_string(),
                );
            }
            None => {}
        }
    }

    fn validate_login_rate_limit(&self, errors: &mut Vec<String>) {
        let rate_limit = &self.auth.login_rate_limit;
        if !rate_limit.enabled {
            return;
        }
        if rate_limit.max_failures == 0 {
            errors.push(
                "auth.login_rate_limit.max_failures must be greater than 0 when enabled"
                    .to_string(),
            );
        } else if rate_limit.max_failures > MAX_LOGIN_RATE_LIMIT_FAILURES {
            errors.push(format!(
                "auth.login_rate_limit.max_failures must be at most {MAX_LOGIN_RATE_LIMIT_FAILURES}"
            ));
        }
        if rate_limit.window_secs == 0 {
            errors.push(
                "auth.login_rate_limit.window_secs must be greater than 0 when enabled".to_string(),
            );
        } else if rate_limit.window_secs > MAX_LOGIN_RATE_LIMIT_WINDOW_SECS {
            errors.push(format!(
                "auth.login_rate_limit.window_secs must be at most {MAX_LOGIN_RATE_LIMIT_WINDOW_SECS}"
            ));
        }
        if rate_limit.lockout_secs == 0 {
            errors.push(
                "auth.login_rate_limit.lockout_secs must be greater than 0 when enabled"
                    .to_string(),
            );
        } else if rate_limit.lockout_secs > MAX_LOGIN_RATE_LIMIT_LOCKOUT_SECS {
            errors.push(format!(
                "auth.login_rate_limit.lockout_secs must be at most {MAX_LOGIN_RATE_LIMIT_LOCKOUT_SECS}"
            ));
        }
        if rate_limit.max_tracked_entries == 0 {
            errors.push(
                "auth.login_rate_limit.max_tracked_entries must be greater than 0 when enabled"
                    .to_string(),
            );
        } else if rate_limit.max_tracked_entries > MAX_LOGIN_RATE_LIMIT_TRACKED_ENTRIES {
            errors.push(format!(
                "auth.login_rate_limit.max_tracked_entries must be at most {MAX_LOGIN_RATE_LIMIT_TRACKED_ENTRIES}"
            ));
        }
    }

    fn validate_oauth(&self, errors: &mut Vec<String>) {
        let oauth = &self.auth.oauth;
        validate_u64_range(
            errors,
            "auth.oauth.timeout_secs",
            oauth.timeout_secs,
            MAX_OAUTH_TIMEOUT_SECS,
        );
        if !oauth.enabled {
            return;
        }

        match parse_url(
            "auth.oauth.issuer_url",
            &oauth.issuer_url,
            &["http", "https"],
        ) {
            Ok(_) => {}
            Err(error) => errors.push(error),
        }
        if oauth.client_id.trim().is_empty() {
            errors.push("auth.oauth.client_id is required when OAuth is enabled".to_string());
        }
        if oauth.client_secret.trim().is_empty() {
            errors.push("auth.oauth.client_secret is required when OAuth is enabled".to_string());
        }
        if !oauth.redirect_url.trim().is_empty() {
            match parse_url(
                "auth.oauth.redirect_url",
                &oauth.redirect_url,
                &["http", "https"],
            ) {
                Ok(_) => {}
                Err(error) => errors.push(error),
            }
        }
        for (index, email) in oauth.allowed_emails.iter().enumerate() {
            if let Err(error) =
                validate_oauth_allowed_email(&format!("auth.oauth.allowed_emails[{index}]"), email)
            {
                errors.push(error);
            }
        }
        for (index, domain) in oauth.allowed_domains.iter().enumerate() {
            if let Err(error) = validate_oauth_allowed_domain(
                &format!("auth.oauth.allowed_domains[{index}]"),
                domain,
            ) {
                errors.push(error);
            }
        }
        if self.server.deployment.is_production()
            && oauth.allowed_emails.is_empty()
            && oauth.allowed_domains.is_empty()
        {
            errors.push(
                "auth.oauth.allowed_emails or auth.oauth.allowed_domains must be configured when OAuth is enabled in production"
                    .to_string(),
            );
        }
        if self.server.deployment.is_production() && !oauth.require_email_verified {
            errors.push(
                "auth.oauth.require_email_verified must be true when OAuth is enabled in production"
                    .to_string(),
            );
        }
    }

    fn validate_redaction(&self, errors: &mut Vec<String>) {
        for (index, header) in self.redaction.sensitive_headers.iter().enumerate() {
            if header.trim().is_empty() || HeaderName::from_bytes(header.trim().as_bytes()).is_err()
            {
                errors.push(format!(
                    "redaction.sensitive_headers[{index}] must be a valid HTTP header name"
                ));
            }
        }
    }

    fn validate_plugins(&self, errors: &mut Vec<String>) {
        if self.plugins.len() > MAX_PLUGINS {
            errors.push(format!(
                "plugins must contain at most {MAX_PLUGINS} entries"
            ));
        }

        let mut names = HashSet::new();
        for (index, plugin) in self.plugins.iter().enumerate() {
            let name = plugin.name.trim();
            if name.is_empty() {
                errors.push(format!("plugins[{index}].name must not be empty"));
            } else if plugin.name != name {
                errors.push(format!(
                    "plugins[{index}].name must not contain leading or trailing whitespace"
                ));
            } else if name.len() > MAX_PLUGIN_NAME_BYTES {
                errors.push(format!(
                    "plugins[{index}].name must be at most {MAX_PLUGIN_NAME_BYTES} bytes"
                ));
            } else if !name.is_ascii() || name.bytes().any(|byte| byte.is_ascii_control()) {
                errors.push(format!(
                    "plugins[{index}].name must contain only printable ASCII"
                ));
            } else if !names.insert(name.to_string()) {
                errors.push(format!("plugins[{index}].name {name:?} is duplicated"));
            }
            if plugin.wasm_path.as_os_str().is_empty() {
                errors.push(format!("plugins[{index}].wasm_path must not be empty"));
            }
            if plugin.hooks.is_empty() {
                errors.push(format!("plugins[{index}].hooks must not be empty"));
            } else if plugin_hooks_have_duplicates(&plugin.hooks) {
                errors.push(format!(
                    "plugins[{index}].hooks must not contain duplicates"
                ));
            }
            if plugin.timeout_ms == 0 {
                errors.push(format!(
                    "plugins[{index}].timeout_ms must be greater than 0"
                ));
            } else if plugin.timeout_ms > MAX_PLUGIN_TIMEOUT_MS {
                errors.push(format!(
                    "plugins[{index}].timeout_ms must be at most {MAX_PLUGIN_TIMEOUT_MS}"
                ));
            }
        }
    }
}

fn plugin_hooks_have_duplicates(hooks: &[PluginHook]) -> bool {
    for (index, hook) in hooks.iter().enumerate() {
        if hooks
            .iter()
            .skip(index + 1)
            .any(|candidate| candidate == hook)
        {
            return true;
        }
    }
    false
}

fn apply_env_overrides(
    config: &mut Config,
    env: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<()> {
    if let Some(value) = env("DATABASE_URL") {
        config.storage.postgres_url = value;
    }
    if let Some(value) = env("LLMTRACE_STORAGE_MAX_CONNECTIONS") {
        config.storage.max_connections = parse_u32_env("LLMTRACE_STORAGE_MAX_CONNECTIONS", &value)?;
    }
    if let Some(value) = env("LLMTRACE_RETENTION_DAYS") {
        config.storage.retention_days = Some(parse_i64_env("LLMTRACE_RETENTION_DAYS", &value)?);
    }
    if let Some(value) = env("LLMTRACE_RETENTION_PRUNE_INTERVAL_SECS") {
        config.storage.retention_prune_interval_secs =
            parse_u64_env("LLMTRACE_RETENTION_PRUNE_INTERVAL_SECS", &value)?;
    }
    if let Some(value) = env("LLMTRACE_RETENTION_PRUNE_BATCH_SIZE") {
        config.storage.retention_prune_batch_size =
            parse_i64_env("LLMTRACE_RETENTION_PRUNE_BATCH_SIZE", &value)?;
    }
    if let Some(value) = env("LLMTRACE_DB_ACQUIRE_TIMEOUT_SECS") {
        config.storage.acquire_timeout_secs =
            parse_u64_env("LLMTRACE_DB_ACQUIRE_TIMEOUT_SECS", &value)?;
    }
    if let Some(value) = env("LLMTRACE_TRACE_QUEUE_CAPACITY") {
        config.storage.trace_queue_capacity =
            parse_usize_env("LLMTRACE_TRACE_QUEUE_CAPACITY", &value)?;
    }
    if let Some(value) = env("LLMTRACE_TRACE_WORKER_COUNT") {
        config.storage.trace_worker_count = parse_usize_env("LLMTRACE_TRACE_WORKER_COUNT", &value)?;
    }
    if let Some(value) = env("LLMTRACE_LISTEN") {
        config.server.listen = value;
    }
    if let Some(value) = env("LLMTRACE_PUBLIC_URL") {
        config.server.public_url = value;
    }
    if let Some(value) = env("LLMTRACE_DEPLOYMENT") {
        config.server.deployment = value.parse()?;
    }
    if let Some(value) = env("LLMTRACE_UI_ENABLED") {
        config.server.ui_enabled = parse_bool_env("LLMTRACE_UI_ENABLED", &value)?;
    }
    if let Some(value) = env("LLMTRACE_DEFAULT_UPSTREAM") {
        config.proxy.default_upstream = value;
    }
    if let Some(value) = env("LLMTRACE_ALLOW_UPSTREAMS") {
        config.proxy.allow_upstreams = parse_csv_env("LLMTRACE_ALLOW_UPSTREAMS", &value)?;
    }
    if let Some(value) = env("LLMTRACE_PROXY_PATH_PREFIXES") {
        config.proxy.path_prefixes = parse_csv_env("LLMTRACE_PROXY_PATH_PREFIXES", &value)?;
    }
    if let Some(value) = env("LLMTRACE_UPSTREAM_HEADER") {
        config.proxy.upstream_header = value;
    }
    if let Some(value) = env("LLMTRACE_PROXY_TIMEOUT_SECS") {
        config.proxy.timeout_secs = parse_u64_env("LLMTRACE_PROXY_TIMEOUT_SECS", &value)?;
    }
    if let Some(value) = env("LLMTRACE_MAX_REQUEST_BODY_BYTES") {
        config.proxy.max_request_body_bytes =
            parse_usize_env("LLMTRACE_MAX_REQUEST_BODY_BYTES", &value)?;
    }
    if let Some(value) = env("LLMTRACE_MAX_RESPONSE_BODY_BYTES") {
        config.proxy.max_response_body_bytes =
            parse_usize_env("LLMTRACE_MAX_RESPONSE_BODY_BYTES", &value)?;
    }
    if let Some(value) = env("LLMTRACE_MAX_WEBSOCKET_MESSAGE_BYTES") {
        config.proxy.max_websocket_message_bytes =
            parse_usize_env("LLMTRACE_MAX_WEBSOCKET_MESSAGE_BYTES", &value)?;
    }
    if let Some(value) = env("LLMTRACE_MAX_WEBSOCKET_SESSION_BYTES") {
        config.proxy.max_websocket_session_bytes =
            parse_usize_env("LLMTRACE_MAX_WEBSOCKET_SESSION_BYTES", &value)?;
    }
    if let Some(value) = env("LLMTRACE_ARCHIVE_STORAGE_BACKEND") {
        config.archive.storage_backend = value.parse()?;
    }
    if let Some(value) = env("LLMTRACE_ARCHIVE_FILESYSTEM_ROOT") {
        config.archive.filesystem_root = PathBuf::from(value);
    }
    if let Some(value) = env("LLMTRACE_ARCHIVE_SEGMENT_UNCOMPRESSED_BYTES") {
        config.archive.segment_uncompressed_bytes =
            parse_usize_env("LLMTRACE_ARCHIVE_SEGMENT_UNCOMPRESSED_BYTES", &value)?;
    }
    if let Some(value) = env("LLMTRACE_ARCHIVE_COMPRESSION_LEVEL") {
        config.archive.compression_level =
            parse_i32_env("LLMTRACE_ARCHIVE_COMPRESSION_LEVEL", &value)?;
    }
    if let Some(value) = env("LLMTRACE_AUTH_COOKIE_SECURE") {
        config.auth.cookie_secure = parse_bool_env("LLMTRACE_AUTH_COOKIE_SECURE", &value)?;
    }
    if let Some(value) = env("LLMTRACE_SESSION_TTL_HOURS") {
        config.auth.session_ttl_hours = parse_i64_env("LLMTRACE_SESSION_TTL_HOURS", &value)?;
    }
    if let Some(value) = env("LLMTRACE_LOGIN_RATE_LIMIT_ENABLED") {
        config.auth.login_rate_limit.enabled =
            parse_bool_env("LLMTRACE_LOGIN_RATE_LIMIT_ENABLED", &value)?;
    }
    if let Some(value) = env("LLMTRACE_LOGIN_RATE_LIMIT_MAX_FAILURES") {
        config.auth.login_rate_limit.max_failures =
            parse_u32_env("LLMTRACE_LOGIN_RATE_LIMIT_MAX_FAILURES", &value)?;
    }
    if let Some(value) = env("LLMTRACE_LOGIN_RATE_LIMIT_WINDOW_SECS") {
        config.auth.login_rate_limit.window_secs =
            parse_u64_env("LLMTRACE_LOGIN_RATE_LIMIT_WINDOW_SECS", &value)?;
    }
    if let Some(value) = env("LLMTRACE_LOGIN_RATE_LIMIT_LOCKOUT_SECS") {
        config.auth.login_rate_limit.lockout_secs =
            parse_u64_env("LLMTRACE_LOGIN_RATE_LIMIT_LOCKOUT_SECS", &value)?;
    }
    if let Some(value) = env("LLMTRACE_LOGIN_RATE_LIMIT_MAX_TRACKED_ENTRIES") {
        config.auth.login_rate_limit.max_tracked_entries =
            parse_usize_env("LLMTRACE_LOGIN_RATE_LIMIT_MAX_TRACKED_ENTRIES", &value)?;
    }
    if let Some(value) = env("LLMTRACE_ADMIN_USERNAME") {
        config.auth.local_admin.username = value;
    }
    if let Some(value) = env("LLMTRACE_ADMIN_PASSWORD") {
        config.auth.local_admin.password = Some(value);
    }
    if let Some(value) = env("LLMTRACE_ADMIN_PASSWORD_HASH") {
        config.auth.local_admin.password_hash = Some(value);
    }
    if let Some(value) = env("LLMTRACE_OAUTH_ENABLED") {
        config.auth.oauth.enabled = parse_bool_env("LLMTRACE_OAUTH_ENABLED", &value)?;
    }
    if let Some(value) = env("LLMTRACE_OAUTH_ISSUER_URL") {
        config.auth.oauth.issuer_url = value;
    }
    if let Some(value) = env("LLMTRACE_OAUTH_CLIENT_ID") {
        config.auth.oauth.client_id = value;
    }
    if let Some(value) = env("LLMTRACE_OAUTH_CLIENT_SECRET") {
        config.auth.oauth.client_secret = value;
    }
    if let Some(value) = env("LLMTRACE_OAUTH_REDIRECT_URL") {
        config.auth.oauth.redirect_url = value;
    }
    if let Some(value) = env("LLMTRACE_OAUTH_TIMEOUT_SECS") {
        config.auth.oauth.timeout_secs = parse_u64_env("LLMTRACE_OAUTH_TIMEOUT_SECS", &value)?;
    }
    if let Some(value) = env("LLMTRACE_OAUTH_REQUIRE_EMAIL_VERIFIED") {
        config.auth.oauth.require_email_verified =
            parse_bool_env("LLMTRACE_OAUTH_REQUIRE_EMAIL_VERIFIED", &value)?;
    }
    if let Some(value) = env("LLMTRACE_OAUTH_ALLOWED_EMAILS") {
        config.auth.oauth.allowed_emails = parse_csv_env("LLMTRACE_OAUTH_ALLOWED_EMAILS", &value)?;
    }
    if let Some(value) = env("LLMTRACE_OAUTH_ALLOWED_DOMAINS") {
        config.auth.oauth.allowed_domains =
            parse_csv_env("LLMTRACE_OAUTH_ALLOWED_DOMAINS", &value)?;
    }
    if let Some(value) = env("LLMTRACE_METRICS_BEARER_TOKEN") {
        config.observability.metrics_bearer_token = Some(value);
    }
    if let Some(value) = env("LLMTRACE_SENSITIVE_HEADERS") {
        config.redaction.sensitive_headers = parse_csv_env("LLMTRACE_SENSITIVE_HEADERS", &value)?;
    }
    if let Some(value) = env("LLMTRACE_STORE_HEADER_HASH") {
        config.redaction.store_header_hash = parse_bool_env("LLMTRACE_STORE_HEADER_HASH", &value)?;
    }
    Ok(())
}

fn validate_u32_range(errors: &mut Vec<String>, field: &str, value: u32, max: u32) {
    if value == 0 {
        errors.push(format!("{field} must be greater than 0"));
    } else if value > max {
        errors.push(format!("{field} must be at most {max}"));
    }
}

fn validate_u64_range(errors: &mut Vec<String>, field: &str, value: u64, max: u64) {
    if value == 0 {
        errors.push(format!("{field} must be greater than 0"));
    } else if value > max {
        errors.push(format!("{field} must be at most {max}"));
    }
}

fn validate_usize_range(errors: &mut Vec<String>, field: &str, value: usize, max: usize) {
    if value == 0 {
        errors.push(format!("{field} must be greater than 0"));
    } else if value > max {
        errors.push(format!("{field} must be at most {max}"));
    }
}

fn parse_url(field: &str, value: &str, allowed_schemes: &[&str]) -> Result<Url, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    let url = Url::parse(value).map_err(|error| format!("{field} is not a valid URL: {error}"))?;
    if url.host_str().is_none() {
        return Err(format!("{field} must include a host"));
    }
    if !allowed_schemes.contains(&url.scheme()) {
        return Err(format!(
            "{field} must use one of these schemes: {}",
            allowed_schemes.join(", ")
        ));
    }
    Ok(url)
}

fn parse_upstream_allow_entry(field: &str, value: &str) -> Result<UpstreamAllowEntry, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{field} must not be empty"));
    }

    if value.contains("://") {
        let url = parse_url(field, value, UPSTREAM_ALLOWLIST_SCHEMES)?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err(format!("{field} must not contain credentials"));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(format!(
                "{field} must not contain a query string or fragment"
            ));
        }
        let host = url
            .host_str()
            .ok_or_else(|| format!("{field} must include a host"))?
            .to_ascii_lowercase();
        let port = url
            .port_or_known_default()
            .ok_or_else(|| format!("{field} must include a known port"))?;
        return Ok(UpstreamAllowEntry::Url {
            scheme: url.scheme().to_string(),
            host,
            port,
            path_prefix: normalize_allowlist_path(url.path()),
        });
    }

    if value
        .bytes()
        .any(|byte| byte.is_ascii_whitespace() || matches!(byte, b'/' | b'?' | b'#' | b'@'))
    {
        return Err(format!(
            "{field} host entries must be hostnames with an optional port"
        ));
    }

    let url = Url::parse(&format!("https://{value}"))
        .map_err(|error| format!("{field} host entry is invalid: {error}"))?;
    let host = url
        .host_str()
        .ok_or_else(|| format!("{field} must include a host"))?
        .to_ascii_lowercase();

    Ok(UpstreamAllowEntry::Host {
        host,
        port: explicit_host_entry_port(value),
    })
}

fn explicit_host_entry_port(value: &str) -> Option<u16> {
    let port = if let Some(rest) = value.strip_prefix('[') {
        let close_bracket = rest.find(']')?;
        rest.get(close_bracket + 1..)?.strip_prefix(':')?
    } else {
        value.rsplit_once(':')?.1
    };

    if port.is_empty() {
        return None;
    }
    port.parse().ok()
}

fn normalize_allowlist_path(path: &str) -> String {
    if path.is_empty() || path == "/" {
        return "/".to_string();
    }
    format!("/{}", path.trim_matches('/'))
}

fn parse_proxy_path_prefix(field: &str, value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    if !value.starts_with('/') {
        return Err(format!("{field} must start with '/', got {value:?}"));
    }
    if value.split('/').any(|segment| segment == "..") {
        return Err(format!("{field} must not contain '..', got {value:?}"));
    }
    let normalized = normalize_allowlist_path(value);
    if normalized == "/" {
        return Err(format!(
            "{field} must not be '/', which would allow all paths to be proxied"
        ));
    }
    Ok(normalized)
}

fn path_prefix_matches(prefix: &str, path: &str) -> bool {
    if prefix == "/" {
        return true;
    }
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn validate_oauth_allowed_email(field: &str, value: &str) -> Result<(), String> {
    let value = validate_oauth_allowlist_value(field, value)?;
    if value.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(format!("{field} must not contain whitespace"));
    }
    let Some((local, domain)) = value.split_once('@') else {
        return Err(format!(
            "{field} must be an email address such as admin@example.com"
        ));
    };
    if local.is_empty() || domain.is_empty() || domain.contains('@') {
        return Err(format!(
            "{field} must be an email address such as admin@example.com"
        ));
    }
    validate_oauth_allowed_domain(field, domain)
        .map_err(|_| format!("{field} must contain a valid domain after @"))
}

fn validate_oauth_allowed_domain(field: &str, value: &str) -> Result<(), String> {
    let value = validate_oauth_allowlist_value(field, value)?;
    if value.len() > 253
        || value.bytes().any(|byte| {
            byte.is_ascii_whitespace()
                || matches!(byte, b'*' | b':' | b'/' | b'?' | b'#' | b'@' | b'[' | b']')
        })
    {
        return Err(format!("{field} must be a domain name such as example.com"));
    }

    for label in value.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(format!("{field} must be a domain name such as example.com"));
        }
    }

    Ok(())
}

fn validate_oauth_allowlist_value<'a>(field: &str, value: &'a str) -> Result<&'a str, String> {
    if value.is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    if value.trim() != value {
        return Err(format!(
            "{field} must not contain leading or trailing whitespace"
        ));
    }
    if !value.is_ascii() || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(format!("{field} must contain only printable ASCII"));
    }
    Ok(value)
}

fn parse_bool_env(name: &str, value: &str) -> anyhow::Result<bool> {
    match value {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        other => anyhow::bail!("{name} must be a boolean value, got {other:?}"),
    }
}

fn parse_u32_env(name: &str, value: &str) -> anyhow::Result<u32> {
    value
        .parse()
        .with_context(|| format!("{name} must be an unsigned integer"))
}

fn parse_i64_env(name: &str, value: &str) -> anyhow::Result<i64> {
    value
        .parse()
        .with_context(|| format!("{name} must be an integer"))
}

fn parse_i32_env(name: &str, value: &str) -> anyhow::Result<i32> {
    value
        .parse()
        .with_context(|| format!("{name} must be an integer"))
}

fn parse_u64_env(name: &str, value: &str) -> anyhow::Result<u64> {
    value
        .parse()
        .with_context(|| format!("{name} must be an unsigned integer"))
}

fn parse_usize_env(name: &str, value: &str) -> anyhow::Result<usize> {
    value
        .parse()
        .with_context(|| format!("{name} must be an unsigned integer"))
}

fn parse_csv_env(name: &str, value: &str) -> anyhow::Result<Vec<String>> {
    let mut items = Vec::new();
    for item in value.split(',') {
        let item = item.trim();
        if item.is_empty() {
            anyhow::bail!("{name} must be a comma-separated list without empty items");
        }
        items.push(item.to_string());
    }
    if items.is_empty() {
        anyhow::bail!("{name} must contain at least one item");
    }
    Ok(items)
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:3000".to_string(),
            public_url: "http://127.0.0.1:3000".to_string(),
            deployment: DeploymentMode::Development,
            ui_enabled: true,
        }
    }
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            default_upstream: "https://api.openai.com".to_string(),
            allow_upstreams: Vec::new(),
            path_prefixes: vec!["/v1".to_string()],
            upstream_header: "x-llmtrace-upstream".to_string(),
            timeout_secs: 300,
            max_request_body_bytes: 64 * 1024 * 1024,
            max_response_body_bytes: 64 * 1024 * 1024,
            max_websocket_message_bytes: 16 * 1024 * 1024,
            max_websocket_session_bytes: 512 * 1024 * 1024,
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            postgres_url: "postgres://postgres:postgres@localhost:5432/llmtrace".to_string(),
            max_connections: 10,
            acquire_timeout_secs: 30,
            trace_queue_capacity: 4096,
            trace_worker_count: 4,
            retention_days: None,
            retention_prune_interval_secs: 3600,
            retention_prune_batch_size: 1000,
        }
    }
}

impl Default for ArchiveConfig {
    fn default() -> Self {
        Self {
            storage_backend: ArchiveStorageBackend::Filesystem,
            filesystem_root: PathBuf::from("spool/archive"),
            segment_uncompressed_bytes: 32 * 1024 * 1024,
            compression_level: 1,
        }
    }
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            cookie_secure: false,
            session_ttl_hours: 24,
            login_rate_limit: LoginRateLimitConfig::default(),
            local_admin: LocalAdminConfig::default(),
            oauth: OAuthConfig::default(),
        }
    }
}

impl Default for LoginRateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_failures: 5,
            window_secs: 300,
            lockout_secs: 900,
            max_tracked_entries: 4096,
        }
    }
}

impl Default for LocalAdminConfig {
    fn default() -> Self {
        Self {
            username: "admin".to_string(),
            password: Some(DEFAULT_LOCAL_ADMIN_PASSWORD.to_string()),
            password_hash: None,
        }
    }
}

impl Default for OAuthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            issuer_url: String::new(),
            client_id: String::new(),
            client_secret: String::new(),
            redirect_url: String::new(),
            timeout_secs: 15,
            require_email_verified: true,
            allowed_emails: Vec::new(),
            allowed_domains: Vec::new(),
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
                "cookie".to_string(),
                "set-cookie".to_string(),
            ],
            store_header_hash: true,
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

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_ARGON2_HASH: &str =
        "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHQ$k9wPtUZeX9pTvvFeUq1eYn5X2IN3QmEF7L7w8zZ3xIQ";

    #[test]
    fn default_development_config_validates() {
        Config::default().validate().unwrap();
    }

    #[test]
    fn production_config_rejects_insecure_defaults() {
        let mut config = Config::default();
        config.server.deployment = DeploymentMode::Production;

        let error = config.validate().unwrap_err().to_string();

        assert!(error.contains("proxy.allow_upstreams must not be empty"));
        assert!(error.contains("storage.retention_days is required"));
        assert!(error.contains("auth.local_admin.password must not be used"));
        assert!(error.contains("observability.metrics_bearer_token is required"));
    }

    #[test]
    fn production_config_requires_login_rate_limit() {
        let mut config = Config::default();
        config.server.deployment = DeploymentMode::Production;
        config.server.public_url = "https://llmtrace.example.com".to_string();
        config.proxy.allow_upstreams = vec!["api.openai.com".to_string()];
        config.storage.retention_days = Some(30);
        config.auth.cookie_secure = true;
        config.auth.local_admin.password = None;
        config.auth.local_admin.password_hash = Some(VALID_ARGON2_HASH.to_string());
        config.auth.login_rate_limit.enabled = false;

        let error = config.validate().unwrap_err().to_string();

        assert!(error.contains("auth.login_rate_limit.enabled must be true"));
    }

    #[test]
    fn production_config_requires_retention_days() {
        let mut config = Config::default();
        config.server.deployment = DeploymentMode::Production;
        config.server.public_url = "https://llmtrace.example.com".to_string();
        config.proxy.allow_upstreams = vec!["api.openai.com".to_string()];
        config.auth.cookie_secure = true;
        config.auth.local_admin.password = None;
        config.auth.local_admin.password_hash = Some(VALID_ARGON2_HASH.to_string());

        let error = config.validate().unwrap_err().to_string();

        assert!(error.contains("storage.retention_days is required"));
    }

    #[test]
    fn production_config_allows_http_reverse_proxy_endpoints() {
        let mut config = production_ready_config();
        config.server.public_url = "http://llmtrace.internal".to_string();
        config.proxy.default_upstream = "http://llm-upstream.internal/v1".to_string();
        config.proxy.allow_upstreams = vec!["http://llm-upstream.internal/v1".to_string()];
        config.auth.cookie_secure = false;

        config.validate().unwrap();
    }

    #[test]
    fn production_config_requires_secure_cookies_for_https_public_url() {
        let mut config = production_ready_config();
        config.auth.cookie_secure = false;

        let error = config.validate().unwrap_err().to_string();

        assert!(error.contains("auth.cookie_secure must be true"));
    }

    #[test]
    fn production_config_allows_http_oauth_redirect_url() {
        let mut config = production_ready_config();
        config.auth.oauth.enabled = true;
        config.auth.oauth.issuer_url = "http://issuer.example.com".to_string();
        config.auth.oauth.client_id = "client".to_string();
        config.auth.oauth.client_secret = "secret".to_string();
        config.auth.oauth.redirect_url =
            "http://llmtrace.example.com/api/auth/oauth/callback".to_string();
        config.auth.oauth.allowed_domains = vec!["example.com".to_string()];

        config.validate().unwrap();
    }

    #[test]
    fn production_config_allows_http_oauth_issuer_url() {
        let mut config = production_ready_config();
        config.auth.oauth.enabled = true;
        config.auth.oauth.issuer_url = "http://issuer.example.com".to_string();
        config.auth.oauth.client_id = "client".to_string();
        config.auth.oauth.client_secret = "secret".to_string();
        config.auth.oauth.allowed_domains = vec!["example.com".to_string()];

        config.validate().unwrap();
    }

    #[test]
    fn production_config_accepts_https_oauth_urls() {
        let mut config = production_ready_config();
        config.auth.oauth.enabled = true;
        config.auth.oauth.issuer_url = "https://issuer.example.com".to_string();
        config.auth.oauth.client_id = "client".to_string();
        config.auth.oauth.client_secret = "secret".to_string();
        config.auth.oauth.redirect_url =
            "https://llmtrace.example.com/api/auth/oauth/callback".to_string();
        config.auth.oauth.allowed_domains = vec!["example.com".to_string()];

        config.validate().unwrap();
    }

    #[test]
    fn oauth_allowlist_accepts_exact_emails_and_domains() {
        let mut config = Config::default();
        config.auth.oauth.enabled = true;
        config.auth.oauth.issuer_url = "http://issuer.example.com".to_string();
        config.auth.oauth.client_id = "client".to_string();
        config.auth.oauth.client_secret = "secret".to_string();
        config.auth.oauth.allowed_emails = vec!["Admin+Prod@Example.COM".to_string()];
        config.auth.oauth.allowed_domains = vec!["Team-1.Example.COM".to_string()];

        config.validate().unwrap();
    }

    #[test]
    fn validation_rejects_invalid_oauth_allowlist_entries() {
        let mut config = Config::default();
        config.auth.oauth.enabled = true;
        config.auth.oauth.issuer_url = "http://issuer.example.com".to_string();
        config.auth.oauth.client_id = "client".to_string();
        config.auth.oauth.client_secret = "secret".to_string();
        config.auth.oauth.allowed_emails = vec![
            " admin@example.com".to_string(),
            "admin @example.com".to_string(),
            "admin".to_string(),
            "admin@example.com@other".to_string(),
        ];
        config.auth.oauth.allowed_domains = vec![
            "https://example.com".to_string(),
            "*.example.com".to_string(),
            "example.com/path".to_string(),
            "-example.com".to_string(),
            "example..com".to_string(),
            "bücher.example".to_string(),
        ];

        let error = config.validate().unwrap_err().to_string();

        assert!(error.contains(
            "auth.oauth.allowed_emails[0] must not contain leading or trailing whitespace"
        ));
        assert!(error.contains("auth.oauth.allowed_emails[1] must not contain whitespace"));
        assert!(error.contains(
            "auth.oauth.allowed_emails[2] must be an email address such as admin@example.com"
        ));
        assert!(error.contains(
            "auth.oauth.allowed_emails[3] must be an email address such as admin@example.com"
        ));
        assert!(
            error.contains(
                "auth.oauth.allowed_domains[0] must be a domain name such as example.com"
            )
        );
        assert!(
            error.contains(
                "auth.oauth.allowed_domains[1] must be a domain name such as example.com"
            )
        );
        assert!(
            error.contains(
                "auth.oauth.allowed_domains[2] must be a domain name such as example.com"
            )
        );
        assert!(
            error.contains(
                "auth.oauth.allowed_domains[3] must be a domain name such as example.com"
            )
        );
        assert!(
            error.contains(
                "auth.oauth.allowed_domains[4] must be a domain name such as example.com"
            )
        );
        assert!(error.contains("auth.oauth.allowed_domains[5] must contain only printable ASCII"));
    }

    #[test]
    fn production_config_requires_verified_oauth_email() {
        let mut config = production_ready_config();
        config.auth.oauth.enabled = true;
        config.auth.oauth.issuer_url = "https://issuer.example.com".to_string();
        config.auth.oauth.client_id = "client".to_string();
        config.auth.oauth.client_secret = "secret".to_string();
        config.auth.oauth.allowed_domains = vec!["example.com".to_string()];
        config.auth.oauth.require_email_verified = false;

        let error = config.validate().unwrap_err().to_string();

        assert!(error.contains("auth.oauth.require_email_verified must be true"));
    }

    #[test]
    fn oauth_config_requires_verified_email_by_default() {
        assert!(OAuthConfig::default().require_email_verified);
    }

    #[test]
    fn validation_rejects_invalid_login_rate_limit_bounds() {
        let mut config = Config::default();
        config.auth.login_rate_limit.max_failures = 0;
        config.auth.login_rate_limit.window_secs = 0;
        config.auth.login_rate_limit.lockout_secs = 0;
        config.auth.login_rate_limit.max_tracked_entries = 0;

        let error = config.validate().unwrap_err().to_string();

        assert!(error.contains("auth.login_rate_limit.max_failures must be greater than 0"));
        assert!(error.contains("auth.login_rate_limit.window_secs must be greater than 0"));
        assert!(error.contains("auth.login_rate_limit.lockout_secs must be greater than 0"));
        assert!(error.contains("auth.login_rate_limit.max_tracked_entries must be greater than 0"));
    }

    #[test]
    fn validation_rejects_excessive_login_rate_limit_bounds() {
        let mut config = Config::default();
        config.auth.login_rate_limit.max_failures = MAX_LOGIN_RATE_LIMIT_FAILURES + 1;
        config.auth.login_rate_limit.window_secs = MAX_LOGIN_RATE_LIMIT_WINDOW_SECS + 1;
        config.auth.login_rate_limit.lockout_secs = MAX_LOGIN_RATE_LIMIT_LOCKOUT_SECS + 1;
        config.auth.login_rate_limit.max_tracked_entries = MAX_LOGIN_RATE_LIMIT_TRACKED_ENTRIES + 1;

        let error = config.validate().unwrap_err().to_string();

        assert!(error.contains("auth.login_rate_limit.max_failures must be at most"));
        assert!(error.contains("auth.login_rate_limit.window_secs must be at most"));
        assert!(error.contains("auth.login_rate_limit.lockout_secs must be at most"));
        assert!(error.contains("auth.login_rate_limit.max_tracked_entries must be at most"));
    }

    #[test]
    fn env_overrides_cover_oauth_and_security_settings() {
        let mut config = Config::default();

        apply_env_overrides(&mut config, |name| {
            match name {
                "LLMTRACE_UI_ENABLED" => Some("false"),
                "LLMTRACE_UPSTREAM_HEADER" => Some("x-upstream"),
                "LLMTRACE_SESSION_TTL_HOURS" => Some("12"),
                "LLMTRACE_OAUTH_ENABLED" => Some("true"),
                "LLMTRACE_OAUTH_ISSUER_URL" => Some("https://issuer.example.com"),
                "LLMTRACE_OAUTH_CLIENT_ID" => Some("client-id"),
                "LLMTRACE_OAUTH_CLIENT_SECRET" => Some("client-secret"),
                "LLMTRACE_OAUTH_REDIRECT_URL" => {
                    Some("https://llmtrace.example.com/api/auth/oauth/callback")
                }
                "LLMTRACE_OAUTH_TIMEOUT_SECS" => Some("12"),
                "LLMTRACE_OAUTH_REQUIRE_EMAIL_VERIFIED" => Some("false"),
                "LLMTRACE_OAUTH_ALLOWED_EMAILS" => Some("admin@example.com, ops@example.com"),
                "LLMTRACE_OAUTH_ALLOWED_DOMAINS" => Some("example.com, internal.example"),
                "LLMTRACE_METRICS_BEARER_TOKEN" => Some("metrics-secret"),
                "LLMTRACE_SENSITIVE_HEADERS" => Some("authorization, x-custom-secret"),
                "LLMTRACE_STORE_HEADER_HASH" => Some("false"),
                "LLMTRACE_ARCHIVE_STORAGE_BACKEND" => Some("postgres"),
                "LLMTRACE_ARCHIVE_FILESYSTEM_ROOT" => Some("/var/lib/llmtrace/archive"),
                "LLMTRACE_ARCHIVE_SEGMENT_UNCOMPRESSED_BYTES" => Some("8388608"),
                "LLMTRACE_ARCHIVE_COMPRESSION_LEVEL" => Some("2"),
                _ => None,
            }
            .map(str::to_string)
        })
        .unwrap();

        assert!(!config.server.ui_enabled);
        assert_eq!(config.proxy.upstream_header, "x-upstream");
        assert_eq!(config.auth.session_ttl_hours, 12);
        assert!(config.auth.oauth.enabled);
        assert_eq!(config.auth.oauth.issuer_url, "https://issuer.example.com");
        assert_eq!(config.auth.oauth.client_id, "client-id");
        assert_eq!(config.auth.oauth.client_secret, "client-secret");
        assert_eq!(
            config.auth.oauth.redirect_url,
            "https://llmtrace.example.com/api/auth/oauth/callback"
        );
        assert_eq!(config.auth.oauth.timeout_secs, 12);
        assert!(!config.auth.oauth.require_email_verified);
        assert_eq!(
            config.auth.oauth.allowed_emails,
            vec![
                "admin@example.com".to_string(),
                "ops@example.com".to_string()
            ]
        );
        assert_eq!(
            config.auth.oauth.allowed_domains,
            vec!["example.com".to_string(), "internal.example".to_string()]
        );
        assert_eq!(
            config.observability.metrics_bearer_token.as_deref(),
            Some("metrics-secret")
        );
        assert_eq!(
            config.redaction.sensitive_headers,
            vec!["authorization".to_string(), "x-custom-secret".to_string()]
        );
        assert!(!config.redaction.store_header_hash);
        assert_eq!(
            config.archive.storage_backend,
            ArchiveStorageBackend::Postgres
        );
        assert_eq!(
            config.archive.filesystem_root,
            PathBuf::from("/var/lib/llmtrace/archive")
        );
        assert_eq!(config.archive.segment_uncompressed_bytes, 8_388_608);
        assert_eq!(config.archive.compression_level, 2);
    }

    #[test]
    fn csv_env_parser_trims_items_and_rejects_empty_entries() {
        let items = parse_csv_env(
            "LLMTRACE_ALLOW_UPSTREAMS",
            "api.openai.com, https://api.example.com/v1",
        )
        .unwrap();

        assert_eq!(
            items,
            vec![
                "api.openai.com".to_string(),
                "https://api.example.com/v1".to_string()
            ]
        );
        assert!(
            parse_csv_env("LLMTRACE_ALLOW_UPSTREAMS", "")
                .unwrap_err()
                .to_string()
                .contains("without empty items")
        );
        assert!(
            parse_csv_env("LLMTRACE_ALLOW_UPSTREAMS", "api.openai.com,")
                .unwrap_err()
                .to_string()
                .contains("without empty items")
        );
    }

    #[test]
    fn production_config_accepts_hardened_settings() {
        let config = production_ready_config();

        config.validate().unwrap();
    }

    #[test]
    fn password_hash_clears_default_plaintext_password() {
        let mut config = Config::default();
        config.auth.local_admin.password_hash = Some(VALID_ARGON2_HASH.to_string());

        config.normalize_sensitive_defaults();

        assert!(config.auth.local_admin.password.is_none());
    }

    #[test]
    fn validation_rejects_bad_operational_bounds() {
        let mut config = Config::default();
        config.proxy.timeout_secs = 0;
        config.proxy.max_request_body_bytes = 0;
        config.proxy.max_response_body_bytes = 0;
        config.proxy.max_websocket_message_bytes = 0;
        config.proxy.max_websocket_session_bytes = 0;
        config.archive.segment_uncompressed_bytes = 0;
        config.archive.compression_level = 0;
        config.storage.max_connections = 0;
        config.storage.acquire_timeout_secs = 0;
        config.storage.trace_queue_capacity = 0;
        config.storage.trace_worker_count = 0;
        config.storage.retention_days = Some(0);
        config.storage.retention_prune_interval_secs = 0;
        config.storage.retention_prune_batch_size = 0;
        config.auth.session_ttl_hours = 0;
        config.auth.oauth.timeout_secs = 0;

        let error = config.validate().unwrap_err().to_string();

        assert!(error.contains("proxy.timeout_secs must be greater than 0"));
        assert!(error.contains("proxy.max_request_body_bytes must be greater than 0"));
        assert!(error.contains("proxy.max_response_body_bytes must be greater than 0"));
        assert!(error.contains("proxy.max_websocket_message_bytes must be greater than 0"));
        assert!(error.contains("proxy.max_websocket_session_bytes must be greater than 0"));
        assert!(error.contains("archive.segment_uncompressed_bytes must be greater than 0"));
        assert!(error.contains("archive.compression_level must be greater than 0"));
        assert!(error.contains("storage.max_connections must be greater than 0"));
        assert!(error.contains("storage.acquire_timeout_secs must be greater than 0"));
        assert!(error.contains("storage.trace_queue_capacity must be greater than 0"));
        assert!(error.contains("storage.trace_worker_count must be greater than 0"));
        assert!(error.contains("storage.retention_days must be greater than 0"));
        assert!(error.contains("storage.retention_prune_interval_secs must be greater than 0"));
        assert!(error.contains("storage.retention_prune_batch_size must be greater than 0"));
        assert!(error.contains("auth.session_ttl_hours must be greater than 0"));
        assert!(error.contains("auth.oauth.timeout_secs must be greater than 0"));
    }

    #[test]
    fn validation_rejects_excessive_operational_bounds() {
        let mut config = Config::default();
        config.proxy.timeout_secs = MAX_PROXY_TIMEOUT_SECS + 1;
        config.proxy.max_request_body_bytes = MAX_REQUEST_BODY_BYTES + 1;
        config.proxy.max_response_body_bytes = MAX_RESPONSE_BODY_BYTES + 1;
        config.proxy.max_websocket_message_bytes = MAX_WEBSOCKET_MESSAGE_BYTES + 1;
        config.proxy.max_websocket_session_bytes = MAX_WEBSOCKET_SESSION_BYTES + 1;
        config.archive.segment_uncompressed_bytes = MAX_ARCHIVE_SEGMENT_UNCOMPRESSED_BYTES + 1;
        config.archive.compression_level = MAX_ARCHIVE_COMPRESSION_LEVEL + 1;
        config.storage.max_connections = MAX_STORAGE_CONNECTIONS + 1;
        config.storage.acquire_timeout_secs = MAX_STORAGE_ACQUIRE_TIMEOUT_SECS + 1;
        config.storage.trace_queue_capacity = MAX_TRACE_QUEUE_CAPACITY + 1;
        config.storage.trace_worker_count = MAX_TRACE_WORKER_COUNT + 1;
        config.storage.retention_days = Some(MAX_RETENTION_DAYS + 1);
        config.storage.retention_prune_interval_secs = MAX_RETENTION_PRUNE_INTERVAL_SECS + 1;
        config.storage.retention_prune_batch_size = MAX_RETENTION_PRUNE_BATCH_SIZE + 1;
        config.auth.session_ttl_hours = MAX_SESSION_TTL_HOURS + 1;
        config.auth.oauth.timeout_secs = MAX_OAUTH_TIMEOUT_SECS + 1;

        let error = config.validate().unwrap_err().to_string();

        assert!(error.contains("proxy.timeout_secs must be at most"));
        assert!(error.contains("proxy.max_request_body_bytes must be at most"));
        assert!(error.contains("proxy.max_response_body_bytes must be at most"));
        assert!(error.contains("proxy.max_websocket_message_bytes must be at most"));
        assert!(error.contains("proxy.max_websocket_session_bytes must be at most"));
        assert!(error.contains("archive.segment_uncompressed_bytes must be at most"));
        assert!(error.contains("archive.compression_level must be at most"));
        assert!(error.contains("storage.max_connections must be at most"));
        assert!(error.contains("storage.acquire_timeout_secs must be at most"));
        assert!(error.contains("storage.trace_queue_capacity must be at most"));
        assert!(error.contains("storage.trace_worker_count must be at most"));
        assert!(error.contains("storage.retention_days must be at most"));
        assert!(error.contains("storage.retention_prune_interval_secs must be at most"));
        assert!(error.contains("storage.retention_prune_batch_size must be at most"));
        assert!(error.contains("auth.session_ttl_hours must be at most"));
        assert!(error.contains("auth.oauth.timeout_secs must be at most"));
    }

    #[test]
    fn validation_rejects_empty_metrics_bearer_token() {
        let mut config = Config::default();
        config.observability.metrics_bearer_token = Some("  ".to_string());

        let error = config.validate().unwrap_err().to_string();

        assert!(error.contains("observability.metrics_bearer_token must not be empty"));
    }

    #[test]
    fn validation_rejects_malformed_metrics_bearer_token() {
        let mut config = Config::default();
        config.observability.metrics_bearer_token = Some("metrics secret".to_string());

        let error = config.validate().unwrap_err().to_string();

        assert!(error.contains(
            "observability.metrics_bearer_token must contain only printable non-whitespace ASCII"
        ));
    }

    #[test]
    fn production_config_requires_metrics_bearer_token() {
        let mut config = production_ready_config();
        config.observability.metrics_bearer_token = None;

        let error = config.validate().unwrap_err().to_string();

        assert!(error.contains("observability.metrics_bearer_token is required"));
    }

    #[test]
    fn upstream_allowlist_host_matches_any_path() {
        let proxy = ProxyConfig {
            allow_upstreams: vec!["api.openai.com".to_string()],
            ..ProxyConfig::default()
        };
        let allowlist = proxy.upstream_allowlist().unwrap();

        assert!(allowlist.allows(&Url::parse("https://api.openai.com/v1/chat").unwrap()));
        assert!(!allowlist.allows(&Url::parse("https://api.example.com/v1/chat").unwrap()));
    }

    #[test]
    fn upstream_allowlist_empty_rejects_unsupported_schemes() {
        let proxy = ProxyConfig {
            allow_upstreams: Vec::new(),
            ..ProxyConfig::default()
        };
        let allowlist = proxy.upstream_allowlist().unwrap();

        assert!(allowlist.allows(&Url::parse("https://api.openai.com/v1/chat").unwrap()));
        assert!(!allowlist.allows(&Url::parse("ftp://api.openai.com/v1/chat").unwrap()));
    }

    #[test]
    fn upstream_allowlist_host_port_constrains_effective_port() {
        let proxy = ProxyConfig {
            allow_upstreams: vec!["api.openai.com:443".to_string()],
            ..ProxyConfig::default()
        };
        let allowlist = proxy.upstream_allowlist().unwrap();

        assert!(allowlist.allows(&Url::parse("https://api.openai.com/v1/chat").unwrap()));
        assert!(!allowlist.allows(&Url::parse("http://api.openai.com/v1/chat").unwrap()));
        assert!(!allowlist.allows(&Url::parse("https://api.openai.com:8443/v1/chat").unwrap()));
    }

    #[test]
    fn upstream_allowlist_url_origin_matches_any_path_on_origin() {
        let proxy = ProxyConfig {
            allow_upstreams: vec!["https://api.openai.com".to_string()],
            ..ProxyConfig::default()
        };
        let allowlist = proxy.upstream_allowlist().unwrap();

        assert!(allowlist.allows(&Url::parse("https://api.openai.com/v1/chat").unwrap()));
        assert!(!allowlist.allows(&Url::parse("http://api.openai.com/v1/chat").unwrap()));
        assert!(!allowlist.allows(&Url::parse("https://api.openai.com:8443/v1/chat").unwrap()));
    }

    #[test]
    fn upstream_allowlist_url_path_prefix_uses_path_boundary() {
        let proxy = ProxyConfig {
            allow_upstreams: vec!["https://api.example.com/v1".to_string()],
            ..ProxyConfig::default()
        };
        let allowlist = proxy.upstream_allowlist().unwrap();

        assert!(allowlist.allows(&Url::parse("https://api.example.com/v1").unwrap()));
        assert!(allowlist.allows(&Url::parse("https://api.example.com/v1/chat").unwrap()));
        assert!(!allowlist.allows(&Url::parse("https://api.example.com/v10/chat").unwrap()));
    }

    #[test]
    fn validation_rejects_invalid_upstream_allowlist_entries() {
        let mut config = Config::default();
        config.proxy.allow_upstreams = vec![
            "https://user:secret@api.example.com".to_string(),
            "https://api.example.com/v1?debug=true".to_string(),
            "api.example.com/path".to_string(),
        ];

        let error = config.validate().unwrap_err().to_string();

        assert!(error.contains("must not contain credentials"));
        assert!(error.contains("must not contain a query string or fragment"));
        assert!(error.contains("host entries must be hostnames with an optional port"));
    }

    #[test]
    fn validation_rejects_excessive_plugin_count() {
        let config = Config {
            plugins: (0..=MAX_PLUGINS)
                .map(|index| valid_plugin_config(&format!("plugin-{index}")))
                .collect(),
            ..Default::default()
        };

        let error = config.validate().unwrap_err().to_string();

        assert!(error.contains("plugins must contain at most"));
    }

    #[test]
    fn validation_rejects_invalid_plugin_bounds() {
        let config = Config {
            plugins: vec![
                PluginConfig {
                    name: " plugin".to_string(),
                    wasm_path: "plugin.wasm".into(),
                    hooks: vec![PluginHook::RequestStart, PluginHook::RequestStart],
                    timeout_ms: 0,
                },
                PluginConfig {
                    name: "plugín".to_string(),
                    wasm_path: std::path::PathBuf::new(),
                    hooks: Vec::new(),
                    timeout_ms: MAX_PLUGIN_TIMEOUT_MS + 1,
                },
                PluginConfig {
                    name: "p".repeat(MAX_PLUGIN_NAME_BYTES + 1),
                    wasm_path: "plugin.wasm".into(),
                    hooks: vec![PluginHook::ResponseEnd],
                    timeout_ms: 50,
                },
            ],
            ..Default::default()
        };

        let error = config.validate().unwrap_err().to_string();

        assert!(error.contains("plugins[0].name must not contain leading or trailing whitespace"));
        assert!(error.contains("plugins[0].hooks must not contain duplicates"));
        assert!(error.contains("plugins[0].timeout_ms must be greater than 0"));
        assert!(error.contains("plugins[1].name must contain only printable ASCII"));
        assert!(error.contains("plugins[1].wasm_path must not be empty"));
        assert!(error.contains("plugins[1].hooks must not be empty"));
        assert!(error.contains("plugins[1].timeout_ms must be at most"));
        assert!(error.contains("plugins[2].name must be at most"));
    }

    #[test]
    fn validation_rejects_credentialed_default_upstream() {
        let mut config = Config::default();
        config.proxy.default_upstream = "https://user:secret@api.example.com".to_string();

        let error = config.validate().unwrap_err().to_string();

        assert!(error.contains("proxy.default_upstream must not contain credentials"));
    }

    #[test]
    fn proxy_path_prefixes_default_allows_v1_paths() {
        let allowlist = ProxyConfig::default().path_prefix_allowlist().unwrap();

        assert!(allowlist.allows("/v1"));
        assert!(allowlist.allows("/v1/chat/completions"));
        assert!(!allowlist.allows("/v10/chat"));
        assert!(!allowlist.allows("/favicon.ico"));
        assert!(!allowlist.allows("/.well-known/acme-challenge/token"));
    }

    #[test]
    fn proxy_path_prefixes_use_path_boundaries() {
        let allowlist = ProxyConfig {
            path_prefixes: vec!["/v1".to_string(), "/llm".to_string()],
            ..ProxyConfig::default()
        }
        .path_prefix_allowlist()
        .unwrap();

        assert!(allowlist.allows("/llm/chat"));
        assert!(!allowlist.allows("/llmtrace"));
    }

    #[test]
    fn production_config_rejects_empty_path_prefixes() {
        let mut config = production_ready_config();
        config.proxy.path_prefixes.clear();

        let error = config.validate().unwrap_err().to_string();

        assert!(error.contains("proxy.path_prefixes must not be empty"));
    }

    #[test]
    fn validation_rejects_root_path_prefix() {
        let mut config = Config::default();
        config.proxy.path_prefixes = vec!["/".to_string()];

        let error = config.validate().unwrap_err().to_string();

        assert!(error.contains("must not be '/'"));
    }

    #[test]
    fn validation_rejects_invalid_path_prefix_entries() {
        let mut config = Config::default();
        config.proxy.path_prefixes = vec!["v1".to_string()];

        let error = config.validate().unwrap_err().to_string();

        assert!(error.contains("must start with '/'"));
    }

    #[test]
    fn path_prefix_allowlist_normalizes_trailing_slashes() {
        let allowlist = ProxyConfig {
            path_prefixes: vec!["/v1/".to_string()],
            ..ProxyConfig::default()
        }
        .path_prefix_allowlist()
        .unwrap();

        assert!(allowlist.allows("/v1"));
        assert!(allowlist.allows("/v1/chat"));
    }

    fn production_ready_config() -> Config {
        let mut config = Config::default();
        config.server.deployment = DeploymentMode::Production;
        config.server.public_url = "https://llmtrace.example.com".to_string();
        config.proxy.allow_upstreams = vec!["api.openai.com".to_string()];
        config.storage.retention_days = Some(30);
        config.auth.cookie_secure = true;
        config.auth.local_admin.password = None;
        config.auth.local_admin.password_hash = Some(VALID_ARGON2_HASH.to_string());
        config.observability.metrics_bearer_token = Some("metrics-secret".to_string());
        config
    }

    fn valid_plugin_config(name: &str) -> PluginConfig {
        PluginConfig {
            name: name.to_string(),
            wasm_path: "plugin.wasm".into(),
            hooks: vec![PluginHook::RequestStart],
            timeout_ms: 50,
        }
    }
}
