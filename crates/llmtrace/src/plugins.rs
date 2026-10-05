use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use wasmtime::{Config, Engine, Linker, Module, Store};

mod cache;
mod http;

use crate::config::{PluginCacheConfig, PluginConfig};
use crate::metrics::RuntimeMetrics;
use crate::types::PluginHook;

/// Wall-clock granularity of the epoch ticker that enforces plugin timeouts.
const EPOCH_TICK: Duration = Duration::from_millis(10);
const MAX_PLUGIN_OUTPUT_BYTES: i32 = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookInput {
    pub hook: PluginHook,
    pub trace_id: String,
    pub method: String,
    pub uri: String,
    pub upstream_url: String,
    pub headers: Value,
    pub body_utf8: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookOutput {
    #[serde(default, alias = "metadata")]
    pub custom_fields: Map<String, Value>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub session_key: Option<String>,
    pub user_id: Option<String>,
    pub user_name: Option<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginStatus {
    pub name: String,
    pub wasm_path: PathBuf,
    pub hooks: Vec<PluginHook>,
    pub loaded: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PluginEffects {
    pub metadata: Map<String, Value>,
    pub tags: Vec<String>,
    pub session_key: Option<String>,
    pub user_id: Option<String>,
    pub user_name: Option<String>,
    pub warnings: Vec<String>,
}

pub struct PluginManager {
    engine: Engine,
    plugins: Vec<Arc<WasmPlugin>>,
    statuses: Vec<PluginStatus>,
    _ticker: Option<EpochTicker>,
    http: reqwest::Client,
    cache: Arc<cache::PluginCache>,
}

struct WasmPlugin {
    name: String,
    wasm_path: PathBuf,
    hooks: Vec<PluginHook>,
    module: Module,
    epoch_deadline: u64,
    timeout_ms: u64,
    http_get_urls: Vec<url::Url>,
}

impl PluginManager {
    pub fn load(
        configs: &[PluginConfig],
        cache_config: &PluginCacheConfig,
        metrics: &RuntimeMetrics,
    ) -> anyhow::Result<Self> {
        let engine = Engine::new(Config::new().epoch_interruption(true))?;
        let mut plugins = Vec::new();
        let mut statuses = Vec::new();

        for config in configs {
            let name = config.name.trim();
            if name.is_empty() {
                statuses.push(PluginStatus {
                    name: "<unnamed>".to_string(),
                    wasm_path: config.wasm_path.clone(),
                    hooks: config.hooks.clone(),
                    loaded: false,
                    error: Some("plugin name is empty".to_string()),
                });
                continue;
            }

            match Module::from_file(&engine, &config.wasm_path) {
                Ok(module) => {
                    plugins.push(Arc::new(WasmPlugin {
                        name: name.to_string(),
                        wasm_path: config.wasm_path.clone(),
                        hooks: config.hooks.clone(),
                        module,
                        epoch_deadline: epoch_deadline(config.timeout_ms),
                        timeout_ms: config.timeout_ms,
                        http_get_urls: config
                            .http_get_urls
                            .iter()
                            .map(|url| url::Url::parse(url))
                            .collect::<Result<_, _>>()?,
                    }));
                    statuses.push(PluginStatus {
                        name: name.to_string(),
                        wasm_path: config.wasm_path.clone(),
                        hooks: config.hooks.clone(),
                        loaded: true,
                        error: None,
                    });
                }
                Err(error) => statuses.push(PluginStatus {
                    name: name.to_string(),
                    wasm_path: config.wasm_path.clone(),
                    hooks: config.hooks.clone(),
                    loaded: false,
                    error: Some(error.to_string()),
                }),
            }
        }

        let ticker = (!plugins.is_empty()).then(|| EpochTicker::spawn(engine.clone()));
        Ok(Self {
            engine,
            plugins,
            statuses,
            _ticker: ticker,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            cache: Arc::new(cache::PluginCache::new(
                cache_config.capacity,
                cache_config.ttl_secs,
                metrics.clone(),
            )),
        })
    }

    pub fn statuses(&self) -> &[PluginStatus] {
        &self.statuses
    }

    pub fn has_plugins(&self) -> bool {
        !self.plugins.is_empty()
    }

    pub fn run_hook(&self, hook: PluginHook, mut input: HookInput) -> PluginEffects {
        let mut effects = PluginEffects::default();
        input.hook = hook;

        for plugin in &self.plugins {
            if !plugin.hooks.contains(&hook) {
                continue;
            }
            match plugin.invoke(&self.engine, &self.http, &self.cache, &input) {
                Ok(Some(output)) => effects.merge(plugin.name.as_str(), output),
                Ok(None) => {}
                Err(error) => effects.warnings.push(format!(
                    "{} ({}): {}",
                    plugin.name,
                    plugin.wasm_path.display(),
                    error
                )),
            }
        }

        effects
    }
}

impl PluginEffects {
    fn merge(&mut self, plugin_name: &str, output: HookOutput) {
        if !output.custom_fields.is_empty() {
            self.metadata
                .insert(plugin_name.to_string(), Value::Object(output.custom_fields));
        }
        self.tags.extend(output.tags);
        self.warnings.extend(output.warnings);
        if self.session_key.is_none() {
            self.session_key = output.session_key;
        }
        if self.user_id.is_none() {
            self.user_id = output.user_id;
        }
        if self.user_name.is_none() {
            self.user_name = output.user_name;
        }
    }
}

impl WasmPlugin {
    fn invoke(
        &self,
        engine: &Engine,
        http: &reqwest::Client,
        cache: &Arc<cache::PluginCache>,
        input: &HookInput,
    ) -> anyhow::Result<Option<HookOutput>> {
        let export_name = format!("llmtrace_{}", input.hook.as_str());
        let mut store = Store::new(
            engine,
            http::HostState::new(
                http.clone(),
                &self.http_get_urls,
                self.timeout_ms,
                cache.clone(),
            ),
        );
        store.limiter(|state| &mut state.limits);
        store.set_epoch_deadline(self.epoch_deadline);

        let mut linker = Linker::new(engine);
        http::register(&mut linker)?;
        cache::register(&mut linker)?;
        let instance = linker.instantiate(&mut store, &self.module)?;
        let Some(func) = instance.get_func(&mut store, &export_name) else {
            return Ok(None);
        };
        let hook_func = func.typed::<(i32, i32), i64>(&mut store)?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| anyhow::anyhow!("plugin does not export memory"))?;
        let alloc = instance.get_typed_func::<i32, i32>(&mut store, "llmtrace_alloc")?;
        let dealloc = instance
            .get_typed_func::<(i32, i32), ()>(&mut store, "llmtrace_dealloc")
            .ok();

        let input_bytes = serde_json::to_vec(input)?;
        let input_len = i32::try_from(input_bytes.len())?;
        let input_ptr = alloc.call(&mut store, input_len)?;
        memory.write(&mut store, input_ptr as usize, &input_bytes)?;
        let packed = hook_func.call(&mut store, (input_ptr, input_bytes.len() as i32))?;
        if let Some(dealloc) = &dealloc {
            let _ = dealloc.call(&mut store, (input_ptr, input_bytes.len() as i32));
        }

        let Some((output_ptr, output_len)) = unpack_plugin_output(packed)? else {
            return Ok(None);
        };

        let mut output_bytes = vec![0_u8; output_len as usize];
        memory.read(&store, output_ptr as usize, &mut output_bytes)?;
        if let Some(dealloc) = &dealloc {
            let _ = dealloc.call(&mut store, (output_ptr, output_len));
        }

        Ok(Some(serde_json::from_slice(&output_bytes)?))
    }
}

fn unpack_plugin_output(packed: i64) -> anyhow::Result<Option<(i32, i32)>> {
    let output_ptr = (packed >> 32) as i32;
    let output_len = (packed & 0xffff_ffff) as i32;
    if output_ptr == 0 || output_len == 0 {
        return Ok(None);
    }
    if output_ptr < 0 {
        anyhow::bail!("plugin returned an invalid output pointer");
    }
    if output_len < 0 {
        anyhow::bail!("plugin returned an invalid output length");
    }
    if output_len > MAX_PLUGIN_OUTPUT_BYTES {
        anyhow::bail!("plugin output exceeds configured limit of {MAX_PLUGIN_OUTPUT_BYTES} bytes");
    }
    Ok(Some((output_ptr, output_len)))
}

/// Number of epoch ticks a plugin invocation may run before it is trapped.
fn epoch_deadline(timeout_ms: u64) -> u64 {
    timeout_ms.div_ceil(EPOCH_TICK.as_millis() as u64).max(1)
}

/// Background thread that advances the engine epoch so blocked plugins are trapped.
struct EpochTicker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl EpochTicker {
    fn spawn(engine: Engine) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_signal = stop.clone();
        let handle = std::thread::spawn(move || {
            while !stop_signal.load(Ordering::Relaxed) {
                std::thread::sleep(EPOCH_TICK);
                engine.increment_epoch();
            }
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for EpochTicker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unpack_plugin_output_treats_zero_pointer_or_length_as_no_output() {
        assert!(unpack_plugin_output(pack_output(0, 16)).unwrap().is_none());
        assert!(unpack_plugin_output(pack_output(16, 0)).unwrap().is_none());
    }

    #[test]
    fn unpack_plugin_output_accepts_output_within_limit() {
        assert_eq!(
            unpack_plugin_output(pack_output(16, MAX_PLUGIN_OUTPUT_BYTES))
                .unwrap()
                .unwrap(),
            (16, MAX_PLUGIN_OUTPUT_BYTES)
        );
    }

    #[test]
    fn unpack_plugin_output_rejects_output_over_limit() {
        let error = unpack_plugin_output(pack_output(16, MAX_PLUGIN_OUTPUT_BYTES + 1))
            .unwrap_err()
            .to_string();

        assert!(error.contains("plugin output exceeds configured limit"));
    }

    #[test]
    fn unpack_plugin_output_rejects_invalid_pointer_or_length() {
        let error = unpack_plugin_output(pack_output(-1, 16))
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid output pointer"));

        let error = unpack_plugin_output(pack_output(16, -1))
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid output length"));
    }

    #[test]
    fn hook_output_accepts_custom_fields() {
        let output: HookOutput = serde_json::from_value(json!({
            "custom_fields": {
                "customer_tier": "enterprise",
                "billing": {"plan": "annual"}
            },
            "tags": ["paid"]
        }))
        .unwrap();
        let mut effects = PluginEffects::default();

        effects.merge("api-key-user-mapper", output);

        assert_eq!(
            effects.metadata.get("api-key-user-mapper"),
            Some(&json!({
                "customer_tier": "enterprise",
                "billing": {"plan": "annual"}
            }))
        );
        assert_eq!(effects.tags, vec!["paid"]);
    }

    #[test]
    fn hook_output_accepts_legacy_metadata_field() {
        let output = serde_json::from_value::<HookOutput>(json!({
            "metadata": {
                "customer_tier": "enterprise"
            }
        }))
        .unwrap();
        assert_eq!(output.custom_fields["customer_tier"], "enterprise");
    }

    fn pack_output(ptr: i32, len: i32) -> i64 {
        ((ptr as i64) << 32) | (len as u32 as i64)
    }

    #[tokio::test]
    #[ignore = "requires the built plugin wasm and permission to bind a local HTTP test server; build with: cargo build -p litellm-user-plugin --target wasm32-unknown-unknown --release"]
    async fn litellm_user_plugin_resolves_identity_and_caches_in_the_plugin_kv()
    -> anyhow::Result<()> {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use axum::{Router, http::HeaderMap, routing::get};

        let key_lookups = Arc::new(AtomicUsize::new(0));
        let user_lookups = Arc::new(AtomicUsize::new(0));
        let key_counter = key_lookups.clone();
        let user_counter = user_lookups.clone();
        let app = Router::new()
            .route(
                "/key/info",
                get(move |headers: HeaderMap| async move {
                    assert_eq!(headers["authorization"], "Bearer sk-test-1234");
                    key_counter.fetch_add(1, Ordering::SeqCst);
                    axum::Json(json!({
                        "key": "sk-test-1234",
                        "info": {
                            "key_alias": "laptop",
                            "team_id": "team-7",
                            "user_id": "authentik-sub-42",
                        }
                    }))
                }),
            )
            .route(
                "/user/info",
                get(move |headers: HeaderMap| async move {
                    assert_eq!(headers["authorization"], "Bearer sk-test-1234");
                    user_counter.fetch_add(1, Ordering::SeqCst);
                    axum::Json(json!({
                        "user_id": "authentik-sub-42",
                        "user_info": {
                            "user_id": "authentik-sub-42",
                            "user_email": "alice@example.com",
                            "user_role": "internal_user",
                            "teams": ["team-7"],
                        },
                        "keys": [],
                        "teams": [],
                    }))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move { axum::serve(listener, app).await });

        let wasm_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/wasm32-unknown-unknown/release/litellm_user_plugin.wasm");
        let config = crate::config::PluginConfig {
            name: "litellm-user".to_string(),
            wasm_path,
            hooks: vec![crate::types::PluginHook::RequestStart],
            timeout_ms: 5_000,
            http_get_urls: vec![
                format!("http://{address}/key/info"),
                format!("http://{address}/user/info"),
            ],
        };
        let metrics = crate::metrics::RuntimeMetrics::default();
        let manager = Arc::new(PluginManager::load(
            &[config],
            &Default::default(),
            &metrics,
        )?);
        anyhow::ensure!(
            manager.statuses().iter().all(|status| status.loaded),
            "plugin wasm failed to load; was it built for wasm32-unknown-unknown?"
        );

        let hook_input = HookInput {
            hook: crate::types::PluginHook::RequestStart,
            trace_id: "0b7b8f8e-1111-4222-8333-444455556666".to_string(),
            method: "POST".to_string(),
            uri: "/v1/chat/completions".to_string(),
            upstream_url: format!("http://{address}/v1/chat/completions"),
            headers: json!({"authorization": "Bearer sk-test-1234"}),
            body_utf8: None,
        };
        let effects = {
            let manager = manager.clone();
            let hook_input = hook_input.clone();
            tokio::task::spawn_blocking(move || {
                manager.run_hook(crate::types::PluginHook::RequestStart, hook_input)
            })
            .await?
        };

        assert!(
            effects.warnings.is_empty(),
            "warnings: {:?}",
            effects.warnings
        );
        assert_eq!(effects.user_id.as_deref(), Some("authentik-sub-42"));
        assert_eq!(effects.user_name.as_deref(), Some("alice@example.com"));
        assert_eq!(effects.metadata["litellm-user"]["key_alias"], "laptop");
        assert_eq!(effects.metadata["litellm-user"]["team_id"], "team-7");
        assert_eq!(
            effects.metadata["litellm-user"]["user_email"],
            "alice@example.com"
        );
        assert_eq!(effects.session_key, None);
        assert_eq!(key_lookups.load(Ordering::SeqCst), 1);
        assert_eq!(user_lookups.load(Ordering::SeqCst), 1);

        // Same credential again: the plugin must serve the cached identity
        // document through cache_get without touching the gateway.
        let effects = {
            let manager = manager.clone();
            tokio::task::spawn_blocking(move || {
                manager.run_hook(crate::types::PluginHook::RequestStart, hook_input)
            })
            .await?
        };
        assert_eq!(effects.user_id.as_deref(), Some("authentik-sub-42"));
        assert_eq!(effects.user_name.as_deref(), Some("alice@example.com"));
        assert_eq!(
            key_lookups.load(Ordering::SeqCst),
            1,
            "key info came from the cache"
        );
        assert_eq!(
            user_lookups.load(Ordering::SeqCst),
            1,
            "user info came from the cache"
        );
        let snapshot = metrics.snapshot(crate::metrics::TraceQueueMetrics::default());
        assert_eq!(snapshot["plugins"]["cache"]["stores"], 1);
        assert_eq!(snapshot["plugins"]["cache"]["hits"], 1);

        server.abort();
        Ok(())
    }
}
