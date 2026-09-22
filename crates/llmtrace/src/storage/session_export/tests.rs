use super::*;
use crate::plugins::PluginManager;
use crate::trace::{TraceEvent, build_trace};

async fn collect_export(export: SessionExport) -> anyhow::Result<Vec<Value>> {
    let (sender, mut receiver) = mpsc::channel(1);
    let task = tokio::spawn(async move { export.write(&sender).await });
    let mut records = Vec::new();
    while let Some(line) = receiver.recv().await {
        records.push(serde_json::from_slice(&line?)?);
    }
    task.await??;
    Ok(records)
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL pointing to a local PostgreSQL test server"]
async fn session_export_preserves_archives_and_snapshot(pool: PgPool) -> anyhow::Result<()> {
    let plugins = PluginManager::load(&[])?;
    let root = std::env::temp_dir().join(format!("llmtrace-export-test-{}", Uuid::new_v4()));
    let request = serde_json::to_vec(&json!({
        "model": "test-model", "metadata": {"session_id": "export-test"},
        "messages": [
            {"role": "system", "content": "instructions\n".repeat(3000)},
            {"role": "user", "content": [{"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}]},
            {"role": "assistant", "tool_calls": [{"id": "call-1", "function": {"name": "read", "arguments": "{}"}}]},
            {"role": "tool", "tool_call_id": "call-1", "content": "tool result\n".repeat(3000)}
        ]
    }))?;
    let response = format!(
        "event: response.output_item.done\ndata: {}\n\ndata: [DONE]\n\n",
        json!({
            "type": "response.output_item.done", "item": {
                "type": "function_call", "call_id": "call-2", "name": "write",
                "arguments": json!({"text": "large tool arguments\n".repeat(3000)}).to_string()
            }
        })
    )
    .into_bytes();
    for (index, backend) in [
        ArchiveStorageBackend::Postgres,
        ArchiveStorageBackend::Filesystem,
    ]
    .into_iter()
    .enumerate()
    {
        let archive = ArchiveConfig {
            storage_backend: backend,
            filesystem_root: root.clone(),
            ..Default::default()
        };
        let started_at = Utc::now();
        let mut ids = Vec::new();
        // Insert in reverse UUID order at the same timestamp to exercise tie ordering.
        for turn in [2, 1] {
            let id = Uuid::from_u128(index as u128 * 10 + turn);
            let mut event = TraceEvent::base(id, started_at);
            event.original_uri = "/v1/responses".into();
            event.upstream_url = "https://llm.example/v1/responses".into();
            event.api_key_hash = Some(format!("key-{index}"));
            event.status = Some(200);
            event.request_body = request.clone();
            event.response_body = response.clone();
            event.request_body_bytes = request.len() as i64;
            event.response_body_bytes = response.len() as i64;
            event.response_body_truncated = turn == 1;
            let (trace, messages, user_id, user_name) = build_trace(event, &plugins)?;
            insert_trace(&pool, &archive, trace, messages, user_id, user_name).await?;
            ids.push(id);
        }
        ids.sort();
        let session: Uuid =
            sqlx::query_scalar("SELECT session_id FROM request_traces WHERE id = $1")
                .bind(ids[0])
                .fetch_one(&pool)
                .await?;
        let export = SessionExport::prepare(&pool, &archive, session)
            .await?
            .unwrap();
        // Retention after the snapshot must not silently omit a request or its blobs.
        sqlx::query("DELETE FROM request_traces WHERE id = $1")
            .bind(ids[1])
            .execute(&pool)
            .await?;
        // Nor may a late insert sneak into the fixed export.
        sqlx::query("INSERT INTO request_traces (id, started_at, method, original_uri, upstream_url, session_id) VALUES ($1, $2, 'POST', '/', 'https://llm.example', $3)")
            .bind(Uuid::new_v4()).bind(started_at - ChronoDuration::minutes(1)).bind(session).execute(&pool).await?;
        let records = collect_export(export).await?;
        assert_eq!(records.len(), 4);
        assert_eq!(records[0]["schema_version"], 1);
        assert_eq!(records[0]["request_count"], 2);
        for (record, id) in records[1..3].iter().zip(&ids) {
            assert_eq!(record["request"]["id"], id.to_string());
            assert_eq!(
                record["request_body"]["data"].as_str().unwrap().as_bytes(),
                request
            );
            assert_eq!(
                record["response_body"]["data"].as_str().unwrap().as_bytes(),
                response
            );
            assert!(
                record["request"]["tags"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("session_messages_truncated"))
            );
            assert!(!record["message_previews"].as_array().unwrap().is_empty());
            assert!(
                record["message_previews"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|message| message["content_truncated"] == true)
            );
        }
        assert_eq!(records[3]["export_complete"], true);
        assert_eq!(records[3]["truncated_body_count"], 1);
        assert_eq!(records[3]["unavailable_body_count"], 0);
        assert_eq!(records[3]["captured_bodies_complete"], false);
        if backend == ArchiveStorageBackend::Filesystem {
            let storage_key: String = sqlx::query_scalar("SELECT s.storage_key FROM payload_archive_segments s JOIN payload_archive_records r ON r.segment_id = s.id WHERE r.trace_id = $1 LIMIT 1")
                .bind(ids[0]).fetch_one(&pool).await?;
            fs::write(archive_file_path(&root, &storage_key)?, b"corrupt archive")?;
            let export = SessionExport::prepare(&pool, &archive, session)
                .await?
                .unwrap();
            let records = collect_export(export).await?;
            let affected = records
                .iter()
                .find(|r| r["request"]["id"] == ids[0].to_string())
                .unwrap();
            assert_eq!(affected["request_body"]["status"], "unreadable");
            assert_eq!(affected["request_body"]["data"], Value::Null);
            assert!(
                records.last().unwrap()["unavailable_body_count"]
                    .as_i64()
                    .unwrap()
                    > 0
            );
        }
    }
    fs::remove_dir_all(root)?;
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL pointing to a local PostgreSQL test server"]
async fn session_export_exceeds_page_limits_and_handles_legacy_bodies(
    pool: PgPool,
) -> anyhow::Result<()> {
    let archive = ArchiveConfig::default();
    let id = Uuid::new_v4();
    assert!(SessionExport::prepare(&pool, &archive, id).await?.is_none());
    sqlx::query("INSERT INTO trace_sessions (id, session_key, first_seen, last_seen) VALUES ($1, 'large-export', now(), now())")
        .bind(id).execute(&pool).await?;
    let empty = collect_export(SessionExport::prepare(&pool, &archive, id).await?.unwrap()).await?;
    assert_eq!(empty.len(), 2);
    assert_eq!(empty[1]["request_count"], 0);
    assert_eq!(empty[1]["captured_bodies_complete"], true);
    let body = br#"{"messages":[{"role":"user","content":"legacy body"}]}"#;
    sqlx::query(
        r#"INSERT INTO request_traces (
        id, started_at, method, original_uri, upstream_url, session_id,
        request_body_compressed, request_body_bytes)
        SELECT md5(i::text)::uuid, now(), 'POST', '/', 'https://llm.example', $1, $2, $3
        FROM generate_series(1, 503) i"#,
    )
    .bind(id)
    .bind(compress(body)?)
    .bind(body.len() as i64)
    .execute(&pool)
    .await?;
    sqlx::query("INSERT INTO session_messages (request_id, session_id, role, content, created_at) SELECT id, session_id, 'user', 'preview', started_at FROM request_traces WHERE session_id = $1")
        .bind(id).execute(&pool).await?;
    sqlx::query(
        "UPDATE request_traces SET request_body_compressed = $1 WHERE id = md5('502')::uuid",
    )
    .bind(Vec::<u8>::new())
    .execute(&pool)
    .await?;
    sqlx::query(
        "UPDATE request_traces SET request_body_compressed = $1 WHERE id = md5('503')::uuid",
    )
    .bind(b"invalid zstd".to_vec())
    .execute(&pool)
    .await?;
    let records =
        collect_export(SessionExport::prepare(&pool, &archive, id).await?.unwrap()).await?;
    assert_eq!(records.len(), 505);
    let footer = records.last().unwrap();
    assert_eq!(footer["request_count"], 503);
    assert_eq!(footer["message_preview_count"], 503);
    assert_eq!(footer["unavailable_body_count"], 2);
    assert_eq!(footer["captured_bodies_complete"], false);
    let mut previous = String::new();
    let mut available = 0;
    for record in &records[1..504] {
        let id = record["request"]["id"].as_str().unwrap();
        assert!(id > previous.as_str());
        previous = id.to_string();
        assert_eq!(
            record["message_previews"][0].get("content_truncated"),
            Some(&Value::Null)
        );
        assert_eq!(record["response_body"]["data"], "");
        if record["request_body"]["status"] == "available" {
            assert_eq!(
                record["request_body"]["data"].as_str().unwrap().as_bytes(),
                body
            );
            available += 1;
        }
    }
    assert_eq!(available, 501);
    // Detail and export must use the same legacy/archive resolution and statuses.
    for record in records
        .iter()
        .filter(|record| record["type"] == "request")
        .take(3)
        .chain(records.iter().filter(|record| {
            record["request_body"]["status"] != "available" && record["type"] == "request"
        }))
    {
        let trace_id = Uuid::parse_str(record["request"]["id"].as_str().unwrap())?;
        let detail = get_request(&pool, trace_id, &archive, true).await?.unwrap();
        assert_eq!(
            detail["request_body_status"],
            record["request_body"]["status"]
        );
        assert_eq!(
            detail["request_body"].as_str().unwrap(),
            record["request_body"]["data"].as_str().unwrap_or_default()
        );
        assert_eq!(detail["response_body_status"], "available");
    }
    Ok(())
}
