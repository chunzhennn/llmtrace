use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::Deserialize;
use wasmtime::{Caller, Linker, StoreLimits, StoreLimitsBuilder};

const MAX_REQUEST_BYTES: usize = 16 * 1024;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

pub(super) struct HostState {
    pub limits: StoreLimits,
    client: reqwest::Client,
    urls: Vec<url::Url>,
    deadline: Instant,
    calls: usize,
    runtime: Option<tokio::runtime::Handle>,
}

impl HostState {
    pub fn new(client: reqwest::Client, urls: &[url::Url], timeout_ms: u64) -> Self {
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
    if !caller.data().urls.contains(&url) {
        anyhow::bail!("URL is not explicitly allowed");
    }
    // Validate the output range before performing external I/O.
    if (out as usize)
        .checked_add(capacity as usize)
        .is_none_or(|end| end > memory.data_size(&*caller))
    {
        anyhow::bail!("invalid output range");
    }
    let mut builder = caller.data().client.get(url);
    for (name, value) in request.headers {
        if matches!(
            name.to_ascii_lowercase().as_str(),
            "host" | "connection" | "content-length" | "transfer-encoding"
        ) {
            anyhow::bail!("unsupported HTTP lookup header");
        }
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
        allowed: Vec<url::Url>,
        timeout_ms: u64,
    ) -> anyhow::Result<Option<Value>> {
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
        let mut store = wasmtime::Store::new(&engine, HostState::new(client, &allowed, timeout_ms));
        store.limiter(|state| &mut state.limits);
        let mut linker = Linker::new(&engine);
        register(&mut linker)?;
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
        let allowed = url::Url::parse("https://identity.example/whoami")?;
        for url in [
            "https://identity.example/other",
            "https://identity.example/whoami?token=secret",
            "https://identity.example.evil/whoami",
            "http://identity.example/whoami",
        ] {
            assert!(lookup(json!({"url": url}), vec![allowed.clone()], 100)?.is_none());
        }
        Ok(())
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
                    assert_eq!(headers["authorization"], "Bearer test-employee-key");
                    axum::Json(json!({"user_id":"employee-1","user_name":"Alice"}))
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
            let result = tokio::task::spawn_blocking(move || lookup(json!({"url": url.as_str(), "headers":{"authorization":"Bearer test-employee-key"}}), vec![url], timeout_ms)).await??;
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
