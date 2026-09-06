use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use wasmtime::{Config, Engine, Linker, Module, Store};

mod http;

use crate::config::PluginConfig;
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
    pub fn load(configs: &[PluginConfig]) -> anyhow::Result<Self> {
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
            match plugin.invoke(&self.engine, &self.http, &input) {
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
        input: &HookInput,
    ) -> anyhow::Result<Option<HookOutput>> {
        let export_name = format!("llmtrace_{}", input.hook.as_str());
        let mut store = Store::new(
            engine,
            http::HostState::new(http.clone(), &self.http_get_urls, self.timeout_ms),
        );
        store.limiter(|state| &mut state.limits);
        store.set_epoch_deadline(self.epoch_deadline);

        let mut linker = Linker::new(engine);
        http::register(&mut linker)?;
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
}
