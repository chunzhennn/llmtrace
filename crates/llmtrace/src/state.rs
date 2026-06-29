use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use reqwest::Client;
use sqlx::PgPool;

use crate::config::Config;
use crate::plugins::PluginManager;
use crate::trace::TraceRecorder;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: PgPool,
    pub http: Client,
    pub plugins: Arc<PluginManager>,
    pub traces: TraceRecorder,
}

impl AppState {
    pub fn new(
        config: Config,
        pool: PgPool,
        plugins: Arc<PluginManager>,
        traces: TraceRecorder,
    ) -> anyhow::Result<Self> {
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(config.proxy.timeout_secs))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("failed to build http client")?;

        Ok(Self {
            config: Arc::new(config),
            pool,
            http,
            plugins,
            traces,
        })
    }
}
