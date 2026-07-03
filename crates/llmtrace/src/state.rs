use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use reqwest::Client;
use sqlx::PgPool;

use crate::config::{Config, PathPrefixAllowlist, UpstreamAllowlist};
use crate::login_throttle::LoginThrottle;
use crate::metrics::RuntimeMetrics;
use crate::plugins::PluginManager;
use crate::trace::TraceRecorder;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: PgPool,
    pub http: Client,
    pub plugins: Arc<PluginManager>,
    pub traces: TraceRecorder,
    pub upstream_allowlist: UpstreamAllowlist,
    pub path_prefixes: PathPrefixAllowlist,
    pub login_throttle: LoginThrottle,
    pub runtime_metrics: RuntimeMetrics,
}

impl AppState {
    pub fn new(
        config: Config,
        pool: PgPool,
        plugins: Arc<PluginManager>,
        traces: TraceRecorder,
        runtime_metrics: RuntimeMetrics,
    ) -> anyhow::Result<Self> {
        let upstream_allowlist = config
            .proxy
            .upstream_allowlist()
            .map_err(|error| anyhow::anyhow!(error))
            .context("failed to build upstream allowlist")?;
        let path_prefixes = config
            .proxy
            .path_prefix_allowlist()
            .map_err(|error| anyhow::anyhow!(error))
            .context("failed to build proxy path prefixes")?;
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(config.proxy.timeout_secs))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("failed to build http client")?;
        let login_throttle = LoginThrottle::new(&config.auth.login_rate_limit);

        Ok(Self {
            config: Arc::new(config),
            pool,
            http,
            plugins,
            traces,
            upstream_allowlist,
            path_prefixes,
            login_throttle,
            runtime_metrics,
        })
    }
}
