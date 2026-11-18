//! Real HTTP recording/indexing fixture for browser acceptance, isolated by SQLx.
use super::*;
use crate::storage;

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires LLMTRACE_NOTE_BROWSER_DIR and a built binary; waits for a stop file"]
async fn performance_note_browser_fixture(pool: PgPool) -> anyhow::Result<()> {
    let Some(directory) = std::env::var_os("LLMTRACE_NOTE_BROWSER_DIR") else {
        return Ok(());
    };
    let directory = PathBuf::from(directory);
    fs::create_dir_all(&directory)?;
    crate::storage::note_index::configure_test_tags(&pool, &["Bench"]).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let upstream_url = format!("http://{}", listener.local_addr()?);
    let mock = tokio::spawn(async move {
        axum::serve(listener, Router::new().fallback(post(|| async {
            axum::Json(json!({"choices":[{"message":{"role":"assistant","content":"Synthetic output [Bench: response-only-excluded]"}}],"usage":{"prompt_tokens":5,"completion_tokens":3}}))
        }))).await
    });
    let mut server = Server::start(
        &pool,
        &upstream_url,
        ArchiveStorageBackend::Postgres,
        0,
        false,
    )
    .await?;
    let client = reqwest::Client::new();
    let note_value = "storage-plan-🙂 %_\\'";
    let mut selected_body = Vec::new();
    for index in 0..54 {
        let content = if index == 53 {
            "Synthetic control without a NOTE".to_owned()
        } else {
            format!("Synthetic before [Bench: {note_value}] after 汉字")
        };
        let body = serde_json::to_vec(
            &json!({"model":"synthetic-model", "metadata":{"session_id":format!("note-browser-{index}")}, "messages":[
                {"role":"system","content":"Synthetic system instructions"}, {"role":"user","content":content}
            ]}),
        )?;
        if index == 0 {
            selected_body = body.clone();
        }
        client
            .post(format!("{}/v1/chat/completions", server.base))
            .header("content-type", "application/json")
            .header("authorization", "Bearer synthetic-note-key")
            .body(body)
            .send()
            .await?
            .error_for_status()?;
    }
    client
        .post(format!("{}/v1/embeddings", server.base))
        .json(&json!({"input":"[Bench: excluded embedding]"}))
        .send()
        .await?
        .error_for_status()?;
    server.settle(&client).await?;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let counts: (i64,i64) = sqlx::query_as("SELECT (SELECT COUNT(*) FROM request_prompt_index_state WHERE state IN ('ready','unsupported')), (SELECT COUNT(*) FROM request_notes)").fetch_one(&pool).await?;
            if counts == (55,53) { break; }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok::<_,anyhow::Error>(())
    }).await??;
    let index_freshness: serde_json::Value = sqlx::query_scalar("SELECT jsonb_build_object('samples', COUNT(*),
        'completion_to_index_ms_p50_p95_p99', percentile_cont(ARRAY[0.5,0.95,0.99]) WITHIN GROUP
        (ORDER BY EXTRACT(EPOCH FROM (i.indexed_at - r.completed_at)) * 1000),
        'scope', 'synthetic_55_request_burst_request_completion_to_index_database_timestamps')
        FROM request_prompt_index_state i JOIN request_traces r ON r.id = i.request_id WHERE i.indexed_at IS NOT NULL AND r.completed_at IS NOT NULL")
        .fetch_one(&pool).await?;
    // Exercise references-only reads on this synthetic fixture after validating
    // occurrence consistency. This is not an attestation for a production DB.
    storage::message_migration::run(
        &pool,
        storage::message_migration::MessageMaintenanceAction::ReferencesOnly,
        1000,
        true,
    )
    .await?;
    storage::message_migration::run(
        &pool,
        storage::message_migration::MessageMaintenanceAction::ClearInline,
        1000,
        false,
    )
    .await?;
    let selected = sqlx::query("SELECT r.id, r.session_id FROM request_traces r JOIN trace_sessions s ON s.id = r.session_id WHERE s.session_key LIKE '%note-browser-0' ORDER BY r.started_at LIMIT 1")
        .fetch_optional(&pool).await?;
    // Session keys can be hashed. The raw body is synthetic and uniquely locates
    // the selected request via its recorded explicit-hint session key in JSON.
    let selected = if let Some(row) = selected {
        row
    } else {
        sqlx::query("SELECT id, session_id FROM request_traces WHERE original_uri = '/v1/chat/completions' ORDER BY started_at, id LIMIT 1").fetch_one(&pool).await?
    };
    fs::write(
        directory.join("fixture.json"),
        serde_json::to_vec_pretty(&json!({
            "synthetic":true, "base":server.base, "note_value":note_value, "matching_sessions":53,
            "request_id":selected.try_get::<Uuid,_>("id")?, "session_id":selected.try_get::<Uuid,_>("session_id")?,
            "request_body":String::from_utf8(selected_body)?, "inline_cleared":true,
            "index_freshness":index_freshness,
        }))?,
    )?;
    let deadline = Instant::now() + Duration::from_secs(3600);
    while !directory.join("stop").exists() && Instant::now() < deadline {
        anyhow::ensure!(server.child.try_wait()?.is_none(), "fixture server exited");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let metrics = server.metrics(&client).await?;
    for name in [
        "dropped_full_total",
        "dropped_memory_total",
        "dropped_closed_total",
        "persist_failures_total",
        "build_failures_total",
    ] {
        anyhow::ensure!(
            metric(&metrics, name) == 0.0,
            "synthetic acceptance capture dropped or failed"
        );
    }
    let root = server.root.clone();
    drop(server);
    fs::remove_dir_all(root)?;
    mock.abort();
    let _ = mock.await;
    Ok(())
}
