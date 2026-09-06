//! Explicitly opted-in, paid provider smoke test. Never run as part of ordinary CI.
use super::*;
use anyhow::ensure;
use std::process::{Child, Command, Stdio};
use std::time::Instant;

struct Fixture {
    root: PathBuf,
    child: Option<Child>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        // The journal temporarily contains original authorization headers.
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "paid OpenRouter smoke test; requires explicit opt-in and a private key file"]
async fn openrouter_live(pool: PgPool) -> anyhow::Result<()> {
    ensure!(
        std::env::var("LLMTRACE_RUN_OPENROUTER").as_deref() == Ok("1"),
        "set LLMTRACE_RUN_OPENROUTER=1 to authorize this paid test"
    );
    let key = fs::read_to_string(std::env::var("OPENROUTER_API_KEY_FILE")?)?;
    let key = key.trim();
    ensure!(!key.is_empty(), "empty OpenRouter credential");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let key_info: Value = client
        .get("https://openrouter.ai/api/v1/key")
        .bearer_auth(key)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let owner = key_info
        .pointer("/data/creator_user_id")
        .and_then(Value::as_str)
        .context("key endpoint has no account-owner identity")?;
    let catalog: Value = client
        .get("https://openrouter.ai/api/v1/models")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let nano = "openai/gpt-4.1-nano";
    let gemini = "google/gemini-2.5-flash-lite";
    let mut config = crate::config::Config::default();
    let mut prices = serde_json::Map::new();
    for name in [nano, gemini] {
        let model = catalog["data"]
            .as_array()
            .context("model catalog missing")?
            .iter()
            .find(|m| m["id"] == name)
            .context("cheap model unavailable")?;
        let rate = |field: &str| -> anyhow::Result<f64> {
            Ok(model["pricing"][field]
                .as_str()
                .context("missing model rate")?
                .parse::<f64>()?
                * 1_000_000.0)
        };
        let input = rate("prompt")?;
        let output = rate("completion")?;
        ensure!(
            input <= 0.11 && output <= 0.41,
            "model price exceeds this test's cheap-model limit"
        );
        config.pricing.insert(
            name.into(),
            crate::pricing::ModelPrice {
                input,
                output,
                cache_read: rate("input_cache_read").ok(),
                cache_write: rate("input_cache_write").ok(),
            },
        );
        prices.insert(
            name.into(),
            json!({"input_per_million":input,"output_per_million":output}),
        );
    }
    let root = std::env::temp_dir().join(format!("llmtrace-openrouter-{}", Uuid::new_v4()));
    fs::create_dir(&root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    }
    let mut fixture = Fixture { root, child: None };
    let socket = std::net::TcpListener::bind("127.0.0.1:0")?;
    config.server.listen = socket.local_addr()?.to_string();
    config.server.public_url = format!("http://{}", config.server.listen);
    config.proxy.default_upstream = "https://openrouter.ai/api".into();
    config.proxy.allow_upstreams = vec!["https://openrouter.ai/api".into()];
    config.proxy.timeout_secs = 60;
    config.archive.filesystem_root = fixture.root.join("archive");
    config.storage.journal.directory = fixture.root.join("journal");
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await?;
    let mut database_url = url::Url::parse(&std::env::var("DATABASE_URL")?)?;
    database_url.set_path(&database);
    config.storage.postgres_url = database_url.to_string();
    let plugin = fixture.root.join("identity.wat");
    fs::write(&plugin, identity_plugin())?;
    config.plugins.push(crate::config::PluginConfig {
        name: "openrouter-identity-test".into(),
        wasm_path: plugin,
        hooks: vec![crate::types::PluginHook::RequestStart],
        timeout_ms: 5000,
        http_get_urls: vec!["https://openrouter.ai/api/v1/key".into()],
    });
    let loaded = crate::plugins::PluginManager::load(&config.plugins)?;
    ensure!(
        loaded.statuses().iter().all(|p| p.loaded),
        "identity test plugin failed to compile"
    );
    drop(loaded);
    let config_path = fixture.root.join("config.toml");
    fs::write(&config_path, toml::to_string(&config)?)?;
    let mut command = Command::new(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/release/llmtrace"),
    );
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("LLMTRACE_")
            || name.to_string_lossy().starts_with("OPENROUTER_")
            || name == "DATABASE_URL"
        {
            command.env_remove(name);
        }
    }
    command
        .arg("--config")
        .arg(config_path)
        .env("RUST_LOG", "error")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    drop(socket);
    fixture.child = Some(command.spawn()?);
    let base = &config.server.public_url;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if client
            .get(format!("{base}/readyz"))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            break;
        }
        ensure!(
            fixture.child.as_mut().unwrap().try_wait()?.is_none(),
            "test proxy exited"
        );
        ensure!(Instant::now() < deadline, "test proxy startup timed out");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    ensure!(
        client
            .get(format!("{base}/api/stats"))
            .send()
            .await?
            .status()
            == 401,
        "statistics were accessible without admin login"
    );
    let login = client
        .post(format!("{base}/api/auth/login"))
        .json(&json!({"username":"admin","password":"admin"}))
        .send()
        .await?
        .error_for_status()?;
    let cookie = login
        .headers()
        .get("set-cookie")
        .context("missing login cookie")?
        .to_str()?
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let tool = json!({"type":"function","function":{"name":"add","description":"Add two integers","parameters":{"type":"object","properties":{"a":{"type":"integer"},"b":{"type":"integer"}},"required":["a","b"],"additionalProperties":false}}});
    let chat = |model: &str, stream: bool| json!({"model":model,"messages":[{"role":"user","content":"Reply with exactly TRACE_OK."}],"max_tokens":32,"temperature":0,"stream":stream,"metadata":{"session_id":"live-chat"}});
    let responses = |stream: bool| json!({"model":nano,"input":[{"role":"user","content":[{"type":"input_text","text":"Reply with exactly TRACE_OK."}]}],"max_output_tokens":32,"temperature":0,"stream":stream,"store":false,"metadata":{"session_id":"live-responses"}});
    let messages = |stream: bool| json!({"model":gemini,"messages":[{"role":"user","content":"Reply with exactly TRACE_OK."}],"max_tokens":32,"temperature":0,"stream":stream,"metadata":{"session_id":"live-messages"}});
    let mut tools = chat(nano, false);
    tools["messages"][0]["content"] =
        json!("Use add to calculate 2 + 3. Do not answer without the tool.");
    tools["tools"] = json!([tool]);
    tools["tool_choice"] = json!({"type":"function","function":{"name":"add"}});
    tools["max_tokens"] = json!(96);
    let mut stream_tools = tools.clone();
    stream_tools["stream"] = json!(true);
    let mut responses_tool = responses(false);
    responses_tool["input"][0]["content"][0]["text"] =
        json!("Use add to calculate 2 + 3. Do not answer without the tool.");
    responses_tool["tools"] = json!([{"type":"function","name":"add","description":"Add two integers","parameters":tool["function"]["parameters"]}]);
    responses_tool["tool_choice"] = json!({"type":"function","name":"add"});
    responses_tool["max_output_tokens"] = json!(96);
    let mut responses_tool_stream = responses_tool.clone();
    responses_tool_stream["stream"] = json!(true);
    let mut messages_tool = messages(false);
    messages_tool["messages"][0]["content"] =
        json!("Use add to calculate 2 + 3. Do not answer without the tool.");
    messages_tool["tools"] = json!([{"name":"add","description":"Add two integers","input_schema":tool["function"]["parameters"]}]);
    messages_tool["tool_choice"] = json!({"type":"tool","name":"add"});
    messages_tool["max_tokens"] = json!(96);
    let mut messages_tool_stream = messages_tool.clone();
    messages_tool_stream["stream"] = json!(true);
    let mut large = chat(gemini, false);
    large["messages"][0]["content"] = json!(format!(
        "Ignore the following padding and reply TRACE_OK.\n{}\nReply TRACE_OK.",
        "sample ".repeat(9000)
    ));
    let mut cases = vec![
        ("chat", "/v1/chat/completions", chat(nano, false)),
        ("chat_stream", "/v1/chat/completions", chat(nano, true)),
        ("gemini_stream", "/v1/chat/completions", chat(gemini, true)),
        ("tool_call", "/v1/chat/completions", tools),
        ("tool_call_stream", "/v1/chat/completions", stream_tools),
        ("responses", "/v1/responses", responses(false)),
        ("responses_stream", "/v1/responses", responses(true)),
        ("messages", "/v1/messages", messages(false)),
        ("messages_stream", "/v1/messages", messages(true)),
        ("large_prompt", "/v1/chat/completions", large),
        ("identity", "/v1/chat/completions", chat(nano, false)),
        (
            "invalid_model",
            "/v1/chat/completions",
            json!({"model":"llmtrace-invalid-model","messages":[{"role":"user","content":"test"}],"max_tokens":1}),
        ),
    ];
    cases.extend([
        ("responses_tool_call", "/v1/responses", responses_tool),
        (
            "responses_tool_call_stream",
            "/v1/responses",
            responses_tool_stream,
        ),
        ("messages_tool_call", "/v1/messages", messages_tool),
        (
            "messages_tool_call_stream",
            "/v1/messages",
            messages_tool_stream,
        ),
    ]);
    if let Ok(selected) = std::env::var("LLMTRACE_OPENROUTER_CASES") {
        let names: Vec<_> = selected.split(',').collect();
        ensure!(
            names
                .iter()
                .all(|name| cases.iter().any(|case| case.0 == *name)),
            "unknown live test case"
        );
        cases.retain(|case| names.contains(&case.0));
        ensure!(!cases.is_empty(), "no live cases selected");
    }
    let mut failures = Vec::new();
    let mut results = Vec::new();
    let mut billed = 0.0;
    let mut session = None;
    let mut tools_reply = None;
    for (name, path, body) in cases.drain(..) {
        // Bounded request count, short outputs, fixed cheap models, no paid server tools.
        ensure!(
            billed < 0.02,
            "stopping live test at its two-cent reported-cost budget"
        );
        let request_bytes = serde_json::to_vec(&body)?;
        let started = Instant::now();
        let mut request = client
            .post(format!("{base}{path}"))
            .bearer_auth(key)
            .header("content-type", "application/json")
            .body(request_bytes.clone());
        if name == "identity" {
            request = request.header("x-llmtrace-test-identity", "1");
        }
        let mut response = request.send().await?;
        let status = response.status().as_u16();
        let id = Uuid::parse_str(
            response
                .headers()
                .get("x-llmtrace-trace-id")
                .with_context(|| format!("missing trace id for {name} (HTTP {status})"))?
                .to_str()?,
        )?;
        let mut raw = Vec::new();
        let mut first_byte_ms = None;
        while let Some(bytes) = response.chunk().await? {
            if !bytes.is_empty() {
                first_byte_ms.get_or_insert(started.elapsed().as_millis());
            }
            raw.extend_from_slice(&bytes);
            ensure!(
                raw.len() < 2 * 1024 * 1024,
                "unexpectedly large provider response"
            );
        }
        let elapsed = started.elapsed().as_millis();
        let events = response_events(&raw);
        let usage = events
            .iter()
            .rev()
            .find_map(|v| v.get("usage").or_else(|| v.pointer("/response/usage")));
        let cost = usage.and_then(|u| u.get("cost")).and_then(Value::as_f64);
        billed += cost.unwrap_or(0.0);
        let deadline = Instant::now() + Duration::from_secs(20);
        let detail = loop {
            if let Some(detail) = get_request(&pool, id, &config.archive, true).await? {
                break detail;
            }
            ensure!(
                Instant::now() < deadline,
                "capture did not persist for {name}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        let mut checks = serde_json::Map::new();
        let mut check = |label: &str, passed: bool| {
            checks.insert(label.into(), json!(passed));
            if !passed {
                failures.push(format!("{name}: {label}"));
            }
        };
        check(
            "expected_http_status",
            if name == "invalid_model" {
                (400..500).contains(&status)
            } else {
                status == 200
            },
        );
        check(
            "exact_request_archive",
            detail["request_body"]
                .as_str()
                .is_some_and(|v| v.as_bytes() == request_bytes),
        );
        check(
            "exact_response_archive",
            detail["response_body"]
                .as_str()
                .is_some_and(|v| v.as_bytes() == raw),
        );
        check(
            "credential_redacted",
            !serde_json::to_string(&detail)?.contains(key),
        );
        check("http_status_recorded", detail["status"] == status);
        check(
            "capture_complete",
            detail["request_body_truncated"] == false && detail["response_body_truncated"] == false,
        );
        if name != "invalid_model" && status == 200 {
            check("no_proxy_error", detail["error"].is_null());
            check(
                "usage_recorded",
                detail["input_tokens"].as_i64().is_some_and(|v| v > 0)
                    && detail["output_tokens"].as_i64().is_some_and(|v| v > 0)
                    && detail["usage_complete"] == true,
            );
            check(
                "cost_estimated",
                detail["estimated_cost_microusd"].as_i64().is_some(),
            );
            check("first_byte_timing", detail["ttfb_ms"].as_i64().is_some());
            check(
                "first_output_timing",
                if body["stream"] == true {
                    detail["ttft_ms"].as_i64().is_some()
                } else {
                    detail["ttft_ms"].is_null()
                },
            );
            if let Some(u) = usage {
                let input = u.get("prompt_tokens").or_else(|| u.get("input_tokens"));
                let output = u
                    .get("completion_tokens")
                    .or_else(|| u.get("output_tokens"));
                if let Some(input) = input {
                    check(
                        "input_usage_matches_provider",
                        detail["input_tokens"] == *input,
                    );
                }
                if let Some(output) = output {
                    check(
                        "output_usage_matches_provider",
                        detail["output_tokens"] == *output,
                    );
                }
            }
            if name.contains("tool_call") {
                check(
                    "tool_name_and_arguments",
                    detail["tool_calls"].as_array().is_some_and(|tools| {
                        tools.iter().any(|t| {
                            t["name"] == "add"
                                && t["arguments"]
                                    .as_str()
                                    .and_then(|a| serde_json::from_str::<Value>(a).ok())
                                    .is_some_and(|a| a["a"] == 2 && a["b"] == 3)
                        })
                    }),
                );
                if name == "tool_call" {
                    tools_reply = events
                        .first()
                        .and_then(|v| v.pointer("/choices/0/message"))
                        .cloned();
                }
            }
            if ["chat", "chat_stream"].contains(&name) {
                if let Some(id) = &session {
                    check("same_conversation_session", detail["session_id"] == *id);
                } else {
                    session = Some(detail["session_id"].clone());
                }
            }
        }
        if name == "identity" {
            let id = Uuid::parse_str(
                detail["session_id"]
                    .as_str()
                    .context("identity request unlinked")?,
            )?;
            let s = get_session(&pool, id, None, None)
                .await?
                .context("identity session absent")?;
            check(
                "account_owner_enrichment",
                s["user_id"] == owner
                    && detail.pointer("/plugin_metadata/openrouter-identity-test/identity_source")
                        == Some(&json!("openrouter_current_key")),
            );
        }
        let row = json!({"case":name,"status":status,"model":detail["model"],"request_kind":detail["request_kind"],"request_bytes":request_bytes.len(),"response_bytes":raw.len(),"client_first_byte_ms":first_byte_ms,"client_duration_ms":elapsed,"ttfb_ms":detail["ttfb_ms"],"ttft_ms":detail["ttft_ms"],"duration_ms":detail["duration_ms"],"input_tokens":detail["input_tokens"],"output_tokens":detail["output_tokens"],"estimated_cost_microusd":detail["estimated_cost_microusd"],"provider_cost_usd":cost,"tool_calls":detail["tool_calls"],"checks":checks,"tags":detail["tags"]});
        eprintln!("LIVE {}", serde_json::to_string(&row)?);
        results.push(row);
    }
    // Complete a real tool round trip using the actual provider-generated call ID.
    if let Some(reply) = tools_reply {
        let call_id = reply
            .pointer("/tool_calls/0/id")
            .and_then(Value::as_str)
            .context("missing tool call id")?;
        let response=client.post(format!("{base}/v1/chat/completions")).bearer_auth(key).json(&json!({"model":nano,"max_tokens":32,"temperature":0,"metadata":{"session_id":"live-chat"},"messages":[{"role":"user","content":"Use add to calculate 2 + 3."},reply,{"role":"tool","tool_call_id":call_id,"content":"5"}]})).send().await?;
        let id = Uuid::parse_str(response.headers()["x-llmtrace-trace-id"].to_str()?)?;
        let status = response.status().as_u16();
        let value: Value = response.json().await?;
        let cost = value
            .pointer("/usage/cost")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        billed += cost;
        let passed = status == 200
            && value
                .pointer("/choices/0/message/content")
                .and_then(Value::as_str)
                .is_some_and(|v| v.contains('5'));
        if !passed {
            failures.push("tool_result: provider did not complete tool round trip".into());
        }
        results.push(json!({"case":"tool_result","status":status,"round_trip_ok":passed,"provider_cost_usd":cost}));
        let deadline = Instant::now() + Duration::from_secs(20);
        while get_request(&pool, id, &config.archive, false)
            .await?
            .is_none()
        {
            ensure!(Instant::now() < deadline, "tool result capture missing");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    let stats: Value = client
        .get(format!("{base}/api/stats"))
        .header("cookie", &cookie)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    ensure!(
        stats["total"].as_u64().unwrap_or(0) > 0,
        "admin statistics missing traces"
    );
    let report = json!({"tested_at":Utc::now(),"model_prices":prices,"reported_generation_cost_usd":billed,"results":results,"failures":failures,"account_owner_lookup_only":true});
    let path = std::env::var("LLMTRACE_OPENROUTER_REPORT")
        .unwrap_or_else(|_| "/tmp/llmtrace-openrouter-report.json".into());
    fs::write(path, serde_json::to_vec_pretty(&report)?)?;
    ensure!(
        failures.is_empty(),
        "live checks failed: {}",
        failures.join(", ")
    );
    Ok(())
}

fn response_events(raw: &[u8]) -> Vec<Value> {
    if let Ok(value) = serde_json::from_slice(raw) {
        return vec![value];
    }
    String::from_utf8_lossy(raw)
        .lines()
        .filter_map(|line| {
            line.strip_prefix("data:")
                .and_then(|data| serde_json::from_str(data.trim()).ok())
        })
        .collect()
}

// Test-only parser for the known ASCII API-key and creator-ID fields. The WAT
// contains no credential; it reads the captured header and performs a real GET.
fn identity_plugin() -> String {
    let values = [
        r#""x-llmtrace-test-identity":"1""#,
        r#""authorization":""#,
        r#"{"url":"https://openrouter.ai/api/v1/key","headers":{"authorization":""#,
        r#""}}"#,
        r#""status":200"#,
        r#"creator_user_id\""#,
        ":",
        "\\\"",
        r#"{"user_id":""#,
        r#"","user_name":"OpenRouter account owner","custom_fields":{"identity_source":"openrouter_current_key"}}"#,
    ];
    let mut offset = 16;
    let mut locations = Vec::new();
    let mut data = String::new();
    for value in values {
        locations.push((offset, value.len()));
        let encoded = value
            .as_bytes()
            .iter()
            .map(|b| format!("\\{b:02x}"))
            .collect::<String>();
        data.push_str(&format!("(data (i32.const {offset}) \"{encoded}\")\n"));
        offset += value.len() + 8;
    }
    let find = |index: usize, ptr: &str, len: &str| {
        format!(
            "(call $find {ptr} {len} (i32.const {}) (i32.const {}))",
            locations[index].0, locations[index].1
        )
    };
    format!(
        r#"(module
      (import "llmtrace" "http_get" (func $get (param i32 i32 i32 i32) (result i32)))
      (memory (export "memory") 64)
      {data}
      (func (export "llmtrace_alloc") (param i32) (result i32) i32.const 131072)
      (func $find (param $p i32) (param $n i32) (param $q i32) (param $m i32) (result i32)
        (local $i i32) (local $j i32)
        (block $none (loop $scan
          (br_if $none (i32.gt_u (i32.add (local.get $i) (local.get $m)) (local.get $n)))
          (local.set $j (i32.const 0))
          (block $next (loop $match
            (if (i32.eq (local.get $j) (local.get $m)) (then (return (i32.add (local.get $p) (local.get $i)))))
            (br_if $next (i32.ne (i32.load8_u (i32.add (local.get $p) (i32.add (local.get $i) (local.get $j)))) (i32.load8_u (i32.add (local.get $q) (local.get $j)))))
            (local.set $j (i32.add (local.get $j) (i32.const 1))) (br $match)))
          (local.set $i (i32.add (local.get $i) (i32.const 1))) (br $scan))) (i32.const -1))
      (func (export "llmtrace_on_request_start") (param $p i32) (param $n i32) (result i64)
        (local $start i32) (local $end i32) (local $len i32) (local $out i32)
        (if (i32.lt_s {marker} (i32.const 0)) (then (return (i64.const 0))))
        (local.set $start {auth})
        (if (i32.lt_s (local.get $start) (i32.const 0)) (then unreachable))
        (local.set $start (i32.add (local.get $start) (i32.const {auth_len})))
        (local.set $end (local.get $start))
        (loop $key (if (i32.ne (i32.load8_u (local.get $end)) (i32.const 34)) (then
          (local.set $end (i32.add (local.get $end) (i32.const 1))) (br $key))))
        (local.set $len (i32.sub (local.get $end) (local.get $start)))
        (if (i32.gt_u (local.get $len) (i32.const 256)) (then unreachable))
        (memory.copy (i32.const 8192) (i32.const {prefix}) (i32.const {prefix_len}))
        (memory.copy (i32.const {key_dest}) (local.get $start) (local.get $len))
        (memory.copy (i32.add (i32.const {key_dest}) (local.get $len)) (i32.const {suffix}) (i32.const {suffix_len}))
        (local.set $out (call $get (i32.const 8192) (i32.add (local.get $len) (i32.const {request_overhead})) (i32.const 65536) (i32.const 65536)))
        (if (i32.lt_s (local.get $out) (i32.const 0)) (then unreachable))
        (if (i32.lt_s {status} (i32.const 0)) (then unreachable))
        (local.set $start {creator})
        (if (i32.lt_s (local.get $start) (i32.const 0)) (then unreachable))
        (local.set $start {colon})
        (local.set $start (i32.add {quote} (i32.const 2)))
        (local.set $end (local.get $start))
        (loop $owner (if (i32.ne (i32.load8_u (local.get $end)) (i32.const 92)) (then
          (local.set $end (i32.add (local.get $end) (i32.const 1))) (br $owner))))
        (local.set $len (i32.sub (local.get $end) (local.get $start)))
        (if (i32.gt_u (local.get $len) (i32.const 128)) (then unreachable))
        (memory.copy (i32.const 16384) (i32.const {output_prefix}) (i32.const {output_prefix_len}))
        (memory.copy (i32.const {output_id}) (local.get $start) (local.get $len))
        (memory.copy (i32.add (i32.const {output_id}) (local.get $len)) (i32.const {output_suffix}) (i32.const {output_suffix_len}))
        (i64.or (i64.const 70368744177664) (i64.extend_i32_u (i32.add (local.get $len) (i32.const {output_overhead}))))
    ))"#,
        marker = find(0, "(local.get $p)", "(local.get $n)"),
        auth = find(1, "(local.get $p)", "(local.get $n)"),
        auth_len = locations[1].1,
        prefix = locations[2].0,
        prefix_len = locations[2].1,
        key_dest = 8192 + locations[2].1,
        suffix = locations[3].0,
        suffix_len = locations[3].1,
        request_overhead = locations[2].1 + locations[3].1,
        status = find(4, "(i32.const 65536)", "(local.get $out)"),
        creator = find(5, "(i32.const 65536)", "(local.get $out)"),
        colon = find(6, "(local.get $start)", "(i32.const 128)"),
        quote = find(7, "(local.get $start)", "(i32.const 128)"),
        output_prefix = locations[8].0,
        output_prefix_len = locations[8].1,
        output_id = 16384 + locations[8].1,
        output_suffix = locations[9].0,
        output_suffix_len = locations[9].1,
        output_overhead = locations[8].1 + locations[9].1
    )
}

#[test]
fn identity_fixture_compiles() -> anyhow::Result<()> {
    wasmtime::Module::new(&wasmtime::Engine::default(), identity_plugin())?;
    Ok(())
}
