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

use crate::types::{BodyRedaction, PluginHook};

const DEFAULT_LOCAL_ADMIN_PASSWORD: &str = "admin";
const MAX_SESSION_TTL_HOURS: i64 = 24 * 30;
const MAX_PLUGIN_TIMEOUT_MS: u64 = 30_000;
const UPSTREAM_ALLOWLIST_SCHEMES: &[&str] = &["http", "https", "ws", "wss"];

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
    pub deployment: DeploymentMode,
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentMode {
    #[default]
    Development,
    Production,
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
        if self.entries.is_empty() {
            return true;
        }
        if !UPSTREAM_ALLOWLIST_SCHEMES.contains(&upstream_url.scheme()) {
            return false;
        }

        self.entries.iter().any(|entry| entry.matches(upstream_url))
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

        if let Ok(value) = std::env::var("DATABASE_URL") {
            config.storage.postgres_url = value;
        }
        if let Ok(value) = std::env::var("LLMTRACE_LISTEN") {
            config.server.listen = value;
        }
        if let Ok(value) = std::env::var("LLMTRACE_PUBLIC_URL") {
            config.server.public_url = value;
        }
        if let Ok(value) = std::env::var("LLMTRACE_DEPLOYMENT") {
            config.server.deployment = value.parse()?;
        }
        if let Ok(value) = std::env::var("LLMTRACE_DEFAULT_UPSTREAM") {
            config.proxy.default_upstream = value;
        }
        if let Ok(value) = std::env::var("LLMTRACE_AUTH_COOKIE_SECURE") {
            config.auth.cookie_secure = parse_bool_env("LLMTRACE_AUTH_COOKIE_SECURE", &value)?;
        }
        if let Ok(value) = std::env::var("LLMTRACE_LOGIN_RATE_LIMIT_ENABLED") {
            config.auth.login_rate_limit.enabled =
                parse_bool_env("LLMTRACE_LOGIN_RATE_LIMIT_ENABLED", &value)?;
        }
        if let Ok(value) = std::env::var("LLMTRACE_LOGIN_RATE_LIMIT_MAX_FAILURES") {
            config.auth.login_rate_limit.max_failures =
                parse_u32_env("LLMTRACE_LOGIN_RATE_LIMIT_MAX_FAILURES", &value)?;
        }
        if let Ok(value) = std::env::var("LLMTRACE_LOGIN_RATE_LIMIT_WINDOW_SECS") {
            config.auth.login_rate_limit.window_secs =
                parse_u64_env("LLMTRACE_LOGIN_RATE_LIMIT_WINDOW_SECS", &value)?;
        }
        if let Ok(value) = std::env::var("LLMTRACE_LOGIN_RATE_LIMIT_LOCKOUT_SECS") {
            config.auth.login_rate_limit.lockout_secs =
                parse_u64_env("LLMTRACE_LOGIN_RATE_LIMIT_LOCKOUT_SECS", &value)?;
        }
        if let Ok(value) = std::env::var("LLMTRACE_LOGIN_RATE_LIMIT_MAX_TRACKED_ENTRIES") {
            config.auth.login_rate_limit.max_tracked_entries =
                parse_usize_env("LLMTRACE_LOGIN_RATE_LIMIT_MAX_TRACKED_ENTRIES", &value)?;
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
        config.normalize_sensitive_defaults();

        Ok(config)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        let mut errors = Vec::new();

        self.validate_server(&mut errors);
        self.validate_proxy(&mut errors);
        self.validate_storage(&mut errors);
        self.validate_auth(&mut errors);
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
            Ok(public_url) => {
                if self.server.deployment.is_production() && public_url.scheme() != "https" {
                    errors.push(
                        "server.public_url must use https when server.deployment is production"
                            .to_string(),
                    );
                }
            }
            Err(error) => errors.push(error),
        }
    }

    fn validate_proxy(&self, errors: &mut Vec<String>) {
        if let Err(error) = parse_url(
            "proxy.default_upstream",
            &self.proxy.default_upstream,
            &["http", "https"],
        ) {
            errors.push(error);
        }

        if self.proxy.upstream_header.trim().is_empty()
            || HeaderName::from_bytes(self.proxy.upstream_header.trim().as_bytes()).is_err()
        {
            errors.push(format!(
                "proxy.upstream_header must be a valid HTTP header name, got {:?}",
                self.proxy.upstream_header
            ));
        }

        if self.proxy.timeout_secs == 0 {
            errors.push("proxy.timeout_secs must be greater than 0".to_string());
        }

        if self.server.deployment.is_production() && self.proxy.allow_upstreams.is_empty() {
            errors.push(
                "proxy.allow_upstreams must not be empty when server.deployment is production"
                    .to_string(),
            );
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
        if self.storage.max_connections == 0 {
            errors.push("storage.max_connections must be greater than 0".to_string());
        }
        if self.storage.trace_queue_capacity == 0 {
            errors.push("storage.trace_queue_capacity must be greater than 0".to_string());
        }
        if self.storage.trace_worker_count == 0 {
            errors.push("storage.trace_worker_count must be greater than 0".to_string());
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
            if !self.auth.cookie_secure {
                errors.push(
                    "auth.cookie_secure must be true when server.deployment is production"
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
        }
        if rate_limit.window_secs == 0 {
            errors.push(
                "auth.login_rate_limit.window_secs must be greater than 0 when enabled".to_string(),
            );
        }
        if rate_limit.lockout_secs == 0 {
            errors.push(
                "auth.login_rate_limit.lockout_secs must be greater than 0 when enabled"
                    .to_string(),
            );
        }
        if rate_limit.max_tracked_entries == 0 {
            errors.push(
                "auth.login_rate_limit.max_tracked_entries must be greater than 0 when enabled"
                    .to_string(),
            );
        }
    }

    fn validate_oauth(&self, errors: &mut Vec<String>) {
        let oauth = &self.auth.oauth;
        if !oauth.enabled {
            return;
        }

        if let Err(error) = parse_url(
            "auth.oauth.issuer_url",
            &oauth.issuer_url,
            &["http", "https"],
        ) {
            errors.push(error);
        }
        if oauth.client_id.trim().is_empty() {
            errors.push("auth.oauth.client_id is required when OAuth is enabled".to_string());
        }
        if oauth.client_secret.trim().is_empty() {
            errors.push("auth.oauth.client_secret is required when OAuth is enabled".to_string());
        }
        if !oauth.redirect_url.trim().is_empty()
            && let Err(error) = parse_url(
                "auth.oauth.redirect_url",
                &oauth.redirect_url,
                &["http", "https"],
            )
        {
            errors.push(error);
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
        if self.server.deployment.is_production()
            && self.redaction.body_redaction == BodyRedaction::Disabled
        {
            errors.push(
                "redaction.body_redaction must be drop or json_secrets when server.deployment is production"
                    .to_string(),
            );
        }
    }

    fn validate_plugins(&self, errors: &mut Vec<String>) {
        let mut names = HashSet::new();
        for (index, plugin) in self.plugins.iter().enumerate() {
            let name = plugin.name.trim();
            if name.is_empty() {
                errors.push(format!("plugins[{index}].name must not be empty"));
            } else if !names.insert(name.to_string()) {
                errors.push(format!("plugins[{index}].name {name:?} is duplicated"));
            }
            if plugin.wasm_path.as_os_str().is_empty() {
                errors.push(format!("plugins[{index}].wasm_path must not be empty"));
            }
            if plugin.hooks.is_empty() {
                errors.push(format!("plugins[{index}].hooks must not be empty"));
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

fn path_prefix_matches(prefix: &str, path: &str) -> bool {
    if prefix == "/" {
        return true;
    }
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
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

        assert!(error.contains("server.public_url must use https"));
        assert!(error.contains("proxy.allow_upstreams must not be empty"));
        assert!(error.contains("auth.cookie_secure must be true"));
        assert!(error.contains("auth.local_admin.password must not be used"));
        assert!(error.contains("redaction.body_redaction must be drop or json_secrets"));
    }

    #[test]
    fn production_config_requires_login_rate_limit() {
        let mut config = Config::default();
        config.server.deployment = DeploymentMode::Production;
        config.server.public_url = "https://llmtrace.example.com".to_string();
        config.proxy.allow_upstreams = vec!["api.openai.com".to_string()];
        config.auth.cookie_secure = true;
        config.auth.local_admin.password = None;
        config.auth.local_admin.password_hash = Some(VALID_ARGON2_HASH.to_string());
        config.auth.login_rate_limit.enabled = false;
        config.redaction.body_redaction = BodyRedaction::JsonSecrets;

        let error = config.validate().unwrap_err().to_string();

        assert!(error.contains("auth.login_rate_limit.enabled must be true"));
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
    fn production_config_accepts_hardened_settings() {
        let mut config = Config::default();
        config.server.deployment = DeploymentMode::Production;
        config.server.public_url = "https://llmtrace.example.com".to_string();
        config.proxy.allow_upstreams = vec!["api.openai.com".to_string()];
        config.auth.cookie_secure = true;
        config.auth.local_admin.password = None;
        config.auth.local_admin.password_hash = Some(VALID_ARGON2_HASH.to_string());
        config.redaction.body_redaction = BodyRedaction::JsonSecrets;

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
        config.storage.max_connections = 0;
        config.storage.trace_queue_capacity = 0;
        config.storage.trace_worker_count = 0;
        config.auth.session_ttl_hours = 0;

        let error = config.validate().unwrap_err().to_string();

        assert!(error.contains("proxy.timeout_secs must be greater than 0"));
        assert!(error.contains("storage.max_connections must be greater than 0"));
        assert!(error.contains("storage.trace_queue_capacity must be greater than 0"));
        assert!(error.contains("storage.trace_worker_count must be greater than 0"));
        assert!(error.contains("auth.session_ttl_hours must be greater than 0"));
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
}
