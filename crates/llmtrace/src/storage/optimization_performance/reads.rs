//! Hot read comparison on the same captures before and after inline cleanup.
use std::time::Instant;

use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::config::{ArchiveConfig, ArchiveStorageBackend};
use crate::metrics::RuntimeMetrics;
use crate::plugins::PluginManager;
use crate::storage;
use crate::storage::message_migration::{self, MessageMaintenanceAction as Action};
use crate::storage::session_export::SessionExport;
use crate::trace::{TraceEvent, build_trace};

async fn sample(pool: &PgPool, archive: &ArchiveConfig, session: Uuid) -> anyhow::Result<Value> {
    let mut report = json!({});
    for (name, offset) in [("detail", 0), ("deep_page", 2400)] {
        let mut times = Vec::new();
        for iteration in 0..33 {
            let started = Instant::now();
            let page = storage::get_session(pool, session, Some(100), Some(offset))
                .await?
                .ok_or_else(|| anyhow::anyhow!("benchmark session missing"))?;
            anyhow::ensure!(
                page["messages"]
                    .as_array()
                    .is_some_and(|rows| rows.len() == 100),
                "benchmark page lost previews"
            );
            if iteration >= 3 {
                times.push(started.elapsed().as_secs_f64() * 1000.0);
            }
        }
        report[name] = json!({"samples":times.len(),"p50_ms":super::percentile(&mut times,0.5),"p95_ms":super::percentile(&mut times,0.95),"p99_ms":super::percentile(&mut times,0.99)});
    }
    let mut times = Vec::new();
    let mut total_bytes = 0_u64;
    for iteration in 0..11 {
        let started = Instant::now();
        let export = SessionExport::prepare(pool, archive, session)
            .await?
            .ok_or_else(|| anyhow::anyhow!("benchmark export missing"))?;
        let (sender, mut receiver) = mpsc::channel(1);
        let task = tokio::spawn(async move { export.write(&sender).await });
        let mut last = None;
        let mut bytes = 0_u64;
        let mut records = 0;
        while let Some(line) = receiver.recv().await {
            let line = line?;
            bytes += line.len() as u64;
            records += 1;
            last = Some(line);
        }
        task.await??;
        let end: Value = serde_json::from_slice(
            last.as_deref()
                .ok_or_else(|| anyhow::anyhow!("missing end marker"))?,
        )?;
        anyhow::ensure!(
            records == 22
                && end["export_complete"] == true
                && end["request_count"] == 20
                && end["message_preview_count"] == 2560,
            "benchmark export incomplete"
        );
        if iteration > 0 {
            times.push(started.elapsed().as_secs_f64() * 1000.0);
            total_bytes += bytes;
        }
    }
    report["full_export"] = json!({"samples":times.len(),"mean_export_bytes":total_bytes / times.len() as u64,
        "p50_ms":super::percentile(&mut times,0.5),"p95_ms":super::percentile(&mut times,0.95),"p99_ms":super::percentile(&mut times,0.99)});
    Ok(report)
}

async fn vacuum_previews(pool: &PgPool) -> anyhow::Result<()> {
    // VACUUM runs separately, outside a transaction, as in the rollout guide.
    for statement in [
        "VACUUM (ANALYZE) session_messages",
        "VACUUM (ANALYZE) message_contents",
    ] {
        sqlx::query(statement).execute(pool).await?;
    }
    Ok(())
}

async fn explain_deep_page(pool: &PgPool, session: Uuid) -> anyhow::Result<Value> {
    let sql = format!(
        "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {}",
        storage::sessions::SESSION_MESSAGES_PAGE_SQL
    );
    Ok(sqlx::query_scalar(&sql)
        .bind(session)
        .bind(101_i64)
        .bind(2400_i64)
        .fetch_one(pool)
        .await?)
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "opt-in hot preview/detail/export comparison on identical captures"]
async fn performance_message_reads(pool: PgPool) -> anyhow::Result<()> {
    message_migration::run(&pool, Action::Legacy, 1000, false).await?;
    let archive = ArchiveConfig {
        storage_backend: ArchiveStorageBackend::Postgres,
        ..Default::default()
    };
    let plugins = PluginManager::load(&[], &Default::default(), &RuntimeMetrics::default())?;
    let messages: Vec<_> = (0..128).map(|index| json!({"role":if index % 2 == 0 {"user"} else {"assistant"},
        "content":(0..64).map(|block| crate::redaction::sha256_hex(format!("synthetic-read-{}-{block}",index % 64).as_bytes())).collect::<String>()})).collect();
    let body = serde_json::to_vec(
        &json!({"metadata":{"session_id":"synthetic-read-session"},"messages":messages}),
    )?;
    let mut first_request = None;
    for _ in 0..20 {
        let mut event = TraceEvent::base(Uuid::new_v4(), Utc::now());
        event.original_uri = "/v1/chat/completions".into();
        event.upstream_url = "https://synthetic.invalid/v1/chat/completions".into();
        event.request_body = body.clone();
        event.request_body_bytes = body.len() as i64;
        let (trace, messages, uid, name) = build_trace(event, &plugins)?;
        first_request.get_or_insert(trace.id);
        storage::insert_trace(&pool, &archive, trace, messages, uid, name).await?;
    }
    let session: Uuid = sqlx::query_scalar("SELECT session_id FROM request_traces WHERE id = $1")
        .bind(first_request)
        .fetch_one(&pool)
        .await?;
    vacuum_previews(&pool).await?;
    let legacy = sample(&pool, &archive, session).await?;
    let legacy_plan = explain_deep_page(&pool, session).await?;
    message_migration::run(&pool, Action::DualWrite, 1000, false).await?;
    while message_migration::run(&pool, Action::Backfill, 1000, false).await?["affected_rows"] != 0
    {
    }
    message_migration::run(&pool, Action::ReferencesOnly, 1000, true).await?;
    while message_migration::run(&pool, Action::ClearInline, 1000, false).await?["affected_rows"]
        != 0
    {}
    sqlx::raw_sql("ANALYZE session_messages; ANALYZE message_contents;")
        .execute(&pool)
        .await?;
    let before_vacuum = sample(&pool, &archive, session).await?;
    vacuum_previews(&pool).await?;
    let references = sample(&pool, &archive, session).await?;
    let reference_plan = explain_deep_page(&pool, session).await?;
    let report = json!({"synthetic":true,"cache":"hot_after_warmup","baseline":"Legacy mode in current binary, identical request IDs/raw archives",
        "requests":20,"previews":2560,"preview_bytes_each":4096,"unique_contents":64,"deep_offset":2400,
        "maintenance_scope":"Legacy and ReferencesOnly include explicit VACUUM (ANALYZE); the immediate post-cleanup sample is retained separately. VACUUM does not promise OS disk reclamation.",
        "legacy":legacy,"references_before_vacuum":before_vacuum,"references_only":references,
        "legacy_deep_page_explain":legacy_plan,"references_only_deep_page_explain":reference_plan});
    let path = std::env::var_os("LLMTRACE_READ_REPORT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("llmtrace-message-reads.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(&report)?)?;
    println!("message read comparison: {}", path.display());
    Ok(())
}
