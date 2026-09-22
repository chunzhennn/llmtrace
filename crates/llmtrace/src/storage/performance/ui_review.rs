//! Isolated, synthetic data for the browser review script. Never uses a live database.
use super::*;
use crate::trace::{TraceEvent, build_trace};

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and LLMTRACE_UI_REVIEW_DIR; waits for a stop file"]
async fn browser_ui_review(pool: PgPool) -> anyhow::Result<()> {
    let Some(directory) = std::env::var_os("LLMTRACE_UI_REVIEW_DIR") else {
        return Ok(());
    };
    let directory = PathBuf::from(directory);
    fs::create_dir_all(&directory)?;
    let plugins = crate::plugins::PluginManager::load(&[])?;
    let archive = ArchiveConfig {
        storage_backend: ArchiveStorageBackend::Postgres,
        ..Default::default()
    };
    let people = ["Maya Chen", "Alex Morgan", "Priya Shah", "Sam Rivera"];
    let prompts = [
        "Review this retry policy for our payments service. Explain how exponential backoff and idempotency keys prevent duplicate charges.",
        "Summarize the customer feedback themes for the quarterly product review. Separate reliability concerns from feature requests.",
        "Help draft an incident update for our internal status page. The database failover completed and we are monitoring recovery.",
        "Find the deployment checklist for the analytics pipeline and explain the rollback steps.",
    ];
    let mut selected = Uuid::nil();
    for index in 0..96 {
        let person = index % people.len();
        let id = Uuid::new_v4();
        let mut event = TraceEvent::base(
            id,
            Utc::now() - ChronoDuration::minutes((95 - index) as i64 * 14),
        );
        let failed = index % 13 == 0;
        let duration = if index % 11 == 0 {
            12340
        } else {
            720 + index as i64 * 23
        };
        event.completed_at = Some(event.started_at + ChronoDuration::milliseconds(duration));
        event.method = "POST".into();
        event.original_uri = "/v1/chat/completions".into();
        event.upstream_url = "https://gateway.example.com/v1/chat/completions".into();
        event.upstream_host = Some("gateway.example.com".into());
        event.api_key_hash = Some(format!("synthetic-user-{person}"));
        event.status = Some(if failed { 429 } else { 200 });
        event.error = failed.then(|| "Upstream rate limit exceeded".into());
        event.duration_ms = Some(duration);
        event.ttft_ms = (!failed).then_some(110 + index as i64 * 2);
        event.ttfb_ms = Some(85 + index as i64);
        event.user_id = Some(format!("user-{person}"));
        event.user_name = Some(people[person].into());
        event.content_type = Some("application/json".into());
        event.request_body = serde_json::to_vec(&json!({
            "model": (["gpt-4.1-mini", "gemini-2.5-flash", "claude-sonnet-4"][index % 3]),
            "metadata": {"session_id": format!("demo-conversation-{}", index % 8)},
            "messages": [{"role":"system", "content":"You are an internal engineering assistant. Use only the synthetic examples provided."},
                         {"role":"user", "content":prompts[person]}]
        }))?;
        event.response_body = serde_json::to_vec(&if failed {
            json!({"error":{"message":"Synthetic rate limit: retry after 30 seconds", "type":"rate_limit_error"}})
        } else {
            json!({"choices":[{"message":{"role":"assistant","content":"Start with a bounded retry budget and exponential backoff with jitter.\n\n1. Reuse the same idempotency key for every retry.\n2. Retry transient failures only, and honor Retry-After.\n3. Record the final outcome and alert on sustained failures.\n\nThis is synthetic review data; no user conversations are included.",
                "tool_calls": if person == 3 {json!([{"id":"demo-search","type":"function","function":{"name":"search_internal_docs","arguments":"{\"query\":\"deployment rollback checklist\"}"}}])} else {json!([])}}, "finish_reason":"stop"}],
                "usage":{"prompt_tokens":1800+index*42,"completion_tokens":320+index*3}})
        })?;
        event.request_body_bytes = event.request_body.len() as i64;
        event.response_body_bytes = event.response_body.len() as i64;
        event.plugin_metadata = json!({"directory":{"department":(["Engineering","Product","Operations","Data"][person])}});
        let (mut trace, messages, uid, name) = build_trace(event, &plugins)?;
        trace.estimated_cost_microusd = (!failed).then_some(4200 + index as i64 * 15);
        insert_trace(&pool, &archive, trace, messages, uid, name).await?;
        selected = id;
    }
    let detail = get_request(&pool, selected, &archive, false)
        .await?
        .unwrap();
    let server = Server::start(
        &pool,
        "http://127.0.0.1:9",
        ArchiveStorageBackend::Postgres,
        0,
        false,
    )
    .await?;
    fs::write(
        directory.join("fixture.json"),
        serde_json::to_vec_pretty(&json!({
            "base":server.base, "request_id":selected, "session_id":detail["session_id"],
            "synthetic":true, "requests":96
        }))?,
    )?;
    let deadline = Instant::now() + Duration::from_secs(3600);
    while !directory.join("stop").exists() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let root = server.root.clone();
    drop(server);
    fs::remove_dir_all(root)?;
    Ok(())
}
