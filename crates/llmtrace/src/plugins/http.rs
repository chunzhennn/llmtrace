use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use wasmtime::{Caller, Linker, StoreLimits, StoreLimitsBuilder};

use super::cache::PluginCache;

const MAX_REQUEST_BYTES: usize = 16 * 1024;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// One allowlisted lookup destination. Plain entries match the full URL
/// exactly, query included. An entry configured with a trailing `?*` keeps
/// scheme/host/port/path exact but accepts any query string, which per-request
/// lookups such as `/key/info?key=<hash>` require.
#[derive(Debug, Clone)]
pub(super) struct AllowedUrl {
    url: url::Url,
    any_query: bool,
}

impl AllowedUrl {
    pub fn parse(entry: &str) -> anyhow::Result<Self> {
        match entry.strip_suffix("?*") {
            Some(base) => {
                let mut url = url::Url::parse(base)?;
                if url.query().is_some() {
                    anyhow::bail!("wildcard entries must not carry their own query");
                }
                url.set_query(None);
                Ok(Self {
                    url,
                    any_query: true,
                })
            }
            None => Ok(Self {
                url: url::Url::parse(entry)?,
                any_query: false,
            }),
        }
    }

    fn allows(&self, candidate: &url::Url) -> bool {
        if !self.any_query {
            return self.url == *candidate;
        }
        let mut stripped = candidate.clone();
        stripped.set_query(None);
        stripped == self.url
    }
}

pub(super) struct HostState {
    pub limits: StoreLimits,
    client: reqwest::Client,
    urls: Vec<AllowedUrl>,
    deadline: Instant,
    calls: usize,
    runtime: Option<tokio::runtime::Handle>,
    pub cache: Arc<PluginCache>,
    /// Injected into every `http_get` from this plugin, overriding any
    /// same-named header the plugin supplies itself.
    pub http_auth: Option<(String, String)>,
}

impl HostState {
    pub fn new(
        client: reqwest::Client,
        urls: &[AllowedUrl],
        timeout_ms: u64,
        cache: Arc<PluginCache>,
        http_auth: Option<(String, String)>,
    ) -> Self {
        Self {
            limits: StoreLimitsBuilder::new()
                .memory_size(128 * 1024 * 1024)
                .table_elements(100_000)
                .instances(1)
                .memories(1)
                .build(),
            client,
            urls: urls.to_vec(),
            deadline: Instant::now() + Duration::from_millis(timeout_ms),
            calls: 0,
            runtime: tokio::runtime::Handle::try_current().ok(),
            cache,
            http_auth,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpGet {
    url: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
}

pub(super) fn register(linker: &mut Linker<HostState>) -> anyhow::Result<()> {
    // Caller owns both buffers. Success returns bytes written; -1 means failure.
    linker.func_wrap(
        "llmtrace",
        "http_get",
        |mut caller: Caller<'_, HostState>, ptr: i32, len: i32, out: i32, capacity: i32| -> i32 {
            http_get(&mut caller, ptr, len, out, capacity).unwrap_or(-1)
        },
    )?;
    Ok(())
}

fn http_get(
    caller: &mut Caller<'_, HostState>,
    ptr: i32,
    len: i32,
    out: i32,
    capacity: i32,
) -> anyhow::Result<i32> {
    if ptr < 0
        || len <= 0
        || len as usize > MAX_REQUEST_BYTES
        || out < 0
        || capacity <= 0
        || capacity as usize > MAX_RESPONSE_BYTES
        || caller.data().calls >= 4
    {
        anyhow::bail!("invalid HTTP host call bounds");
    }
    caller.data_mut().calls += 1;
    let memory = caller
        .get_export("memory")
        .and_then(|export| export.into_memory())
        .ok_or_else(|| anyhow::anyhow!("missing memory"))?;
    let mut request = vec![0; len as usize];
    memory.read(&*caller, ptr as usize, &mut request)?;
    let request: HttpGet = serde_json::from_slice(&request)?;
    let url = url::Url::parse(&request.url)?;
    if !caller
        .data()
        .urls
        .iter()
        .any(|allowed| allowed.allows(&url))
    {
        anyhow::bail!("URL is not explicitly allowed");
    }
    // Validate the output range before performing external I/O.
    if (out as usize)
        .checked_add(capacity as usize)
        .is_none_or(|end| end > memory.data_size(&*caller))
    {
        anyhow::bail!("invalid output range");
    }
    let injected_auth = caller
        .data()
        .http_auth
        .as_ref()
        .map(|(name, value)| (name.to_ascii_lowercase(), name.clone(), value.clone()));
    let mut builder = caller.data().client.get(url);
    for (name, value) in request.headers {
        if matches!(
            name.to_ascii_lowercase().as_str(),
            "host" | "connection" | "content-length" | "transfer-encoding"
        ) {
            anyhow::bail!("unsupported HTTP lookup header");
        }
        if injected_auth
            .as_ref()
            .is_some_and(|(lower, _, _)| *lower == name.to_ascii_lowercase())
        {
            // The host's service credential wins over a plugin-supplied
            // same-named header.
            continue;
        }
        builder = builder.header(
            reqwest::header::HeaderName::from_bytes(name.as_bytes())?,
            reqwest::header::HeaderValue::from_str(&value)?,
        );
    }
    if let Some((_, name, value)) = injected_auth {
        builder = builder.header(
            reqwest::header::HeaderName::from_bytes(name.as_bytes())?,
            reqwest::header::HeaderValue::from_str(&value)?,
        );
    }
    let timeout = caller
        .data()
        .deadline
        .saturating_duration_since(Instant::now());
    let runtime = caller
        .data()
        .runtime
        .clone()
        .ok_or_else(|| anyhow::anyhow!("HTTP host requires background runtime"))?;
    let bytes = runtime.block_on(async move {
        tokio::time::timeout(timeout, async move {
            let mut response = builder.send().await?;
            let status = response.status().as_u16();
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                    anyhow::bail!("lookup body too large");
                }
                body.extend_from_slice(&chunk);
            }
            let body = String::from_utf8(body)?;
            Ok::<_, anyhow::Error>(serde_json::to_vec(
                &serde_json::json!({"status": status, "body": body}),
            )?)
        })
        .await?
    })?;
    if bytes.len() > capacity as usize {
        anyhow::bail!("lookup response exceeds output capacity");
    }
    memory.write(caller, out as usize, &bytes)?;
    Ok(bytes.len() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn lookup(
        request: Value,
        allowed: Vec<&str>,
        timeout_ms: u64,
    ) -> anyhow::Result<Option<Value>> {
        let allowed: Vec<AllowedUrl> = allowed
            .iter()
            .map(|entry| AllowedUrl::parse(entry))
            .collect::<Result<_, _>>()?;
        let bytes = serde_json::to_vec(&request)?;
        let data = bytes
            .iter()
            .map(|byte| format!("\\{byte:02x}"))
            .collect::<String>();
        let wat = format!(
            r#"(module
            (import "llmtrace" "http_get" (func $get (param i32 i32 i32 i32) (result i32)))
            (memory (export "memory") 2)
            (data (i32.const 16) "{data}")
            (func (export "run") (result i32)
                i32.const 16 i32.const {} i32.const 32768 i32.const 65536 call $get))"#,
            bytes.len()
        );
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, wat)?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let mut store = wasmtime::Store::new(
            &engine,
            HostState::new(
                client,
                &allowed,
                timeout_ms,
                Arc::new(PluginCache::new(
                    0,
                    1,
                    crate::metrics::RuntimeMetrics::default(),
                )),
                None,
            ),
        );
        store.limiter(|state| &mut state.limits);
        let mut linker = Linker::new(&engine);
        register(&mut linker)?;
        crate::plugins::cache::register(&mut linker)?;
        let instance = linker.instantiate(&mut store, &module)?;
        let run = instance.get_typed_func::<(), i32>(&mut store, "run")?;
        let len = run.call(&mut store, ())?;
        if len < 0 {
            return Ok(None);
        }
        let mut result = vec![0; len as usize];
        instance
            .get_memory(&mut store, "memory")
            .unwrap()
            .read(&store, 32768, &mut result)?;
        Ok(Some(serde_json::from_slice(&result)?))
    }

    #[test]
    fn http_host_denies_unlisted_urls_before_io() -> anyhow::Result<()> {
        for url in [
            "https://identity.example/other",
            "https://identity.example/whoami?token=secret",
            "https://identity.example.evil/whoami",
            "http://identity.example/whoami",
        ] {
            assert!(
                lookup(
                    json!({"url": url}),
                    vec!["https://identity.example/whoami"],
                    100
                )?
                .is_none()
            );
        }
        Ok(())
    }

    #[test]
    fn wildcard_entries_accept_any_query_but_pin_the_path() -> anyhow::Result<()> {
        // The wildcard relaxes only the query; paths and origins stay exact.
        assert!(
            lookup(
                json!({"url": "https://identity.example/whoami?key=abc"}),
                vec!["https://identity.example/whoami?*"],
                100
            )?
            .is_none(),
            "no server is running; a request that passes the allowlist fails on transport"
        );
        for url in [
            "https://identity.example/other?key=abc",
            "https://identity.example.evil/whoami?key=abc",
        ] {
            assert!(
                lookup(
                    json!({"url": url}),
                    vec!["https://identity.example/whoami?*"],
                    100
                )?
                .is_none()
            );
        }
        // Without the wildcard marker the query must match exactly, so an
        // extra parameter is denied before any I/O.
        assert!(
            lookup(
                json!({"url": "https://identity.example/whoami?key=abc"}),
                vec!["https://identity.example/whoami"],
                100
            )?
            .is_none()
        );
        Ok(())
    }

    #[test]
    fn wildcard_entry_parsing_rejects_own_queries() {
        assert!(AllowedUrl::parse("https://h.example/p?a=1?*").is_err());
        assert!(AllowedUrl::parse("https://h.example/p?*").is_ok());
        assert!(AllowedUrl::parse("https://h.example/p").is_ok());
    }

    #[tokio::test]
    #[ignore = "requires permission to bind a local HTTP test server"]
    async fn http_host_looks_up_identity_without_following_redirects_and_times_out()
    -> anyhow::Result<()> {
        use axum::{Router, http::HeaderMap, routing::get};
        let app = Router::new()
            .route(
                "/whoami",
                get(|headers: HeaderMap| async move {
                    assert_eq!(headers["authorization"], "Bearer test-user-key");
                    axum::Json(json!({"user_id":"user-1","user_name":"Alice"}))
                }),
            )
            .route(
                "/redirect",
                get(|| async { axum::response::Redirect::temporary("/must-not-follow") }),
            )
            .route(
                "/must-not-follow",
                get(|| async {
                    panic!("lookup followed a redirect");
                    #[allow(unreachable_code)]
                    ""
                }),
            )
            .route(
                "/slow",
                get(|| async {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    "late"
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        for path in ["whoami", "redirect", "slow"] {
            let url = url::Url::parse(&format!("http://{address}/{path}"))?;
            let timeout_ms = if path == "slow" { 50 } else { 2000 };
            let allowed = format!("http://{address}/{path}");
            let result = tokio::task::spawn_blocking(move || lookup(json!({"url": url.as_str(), "headers":{"authorization":"Bearer test-user-key"}}), vec![allowed.as_str()], timeout_ms)).await??;
            match path {
                "whoami" => {
                    let result = result.unwrap();
                    assert_eq!(result["status"], 200);
                    let identity: Value = serde_json::from_str(result["body"].as_str().unwrap())?;
                    assert_eq!(identity["user_name"], "Alice");
                }
                "redirect" => assert_eq!(result.unwrap()["status"], 307),
                _ => assert!(result.is_none()),
            }
        }
        server.abort();
        Ok(())
    }
}
