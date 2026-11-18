//! Actual archive/session/rollup ingestion timing; not bulk-SQL throughput.
use std::time::Instant;

use chrono::Utc;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::{ArchiveConfig, ArchiveStorageBackend};
use crate::metrics::RuntimeMetrics;
use crate::plugins::PluginManager;
use crate::storage;
use crate::storage::message_migration::{self, MessageMaintenanceAction as Action};
use crate::trace::{TraceEvent, build_trace};

#[sqlx::test(migrations = "./migrations")]
#[ignore = "opt-in synthetic actual-ingestion and compression benchmark"]
async fn performance_preview_ingestion_modes(pool: PgPool) -> anyhow::Result<()> {
    let samples = std::env::var("LLMTRACE_INGEST_SAMPLES")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(100);
    anyhow::ensure!(
        (20..=500).contains(&samples),
        "ingestion samples must be 20..=500"
    );
    let plugins = PluginManager::load(&[], &Default::default(), &RuntimeMetrics::default())?;
    let archive = ArchiveConfig {
        storage_backend: ArchiveStorageBackend::Postgres,
        ..Default::default()
    };
    let mut texts = Vec::new();
    for message in 0..64 {
        let mut content = String::new();
        for block in 0..64 {
            content.push_str(&crate::redaction::sha256_hex(
                format!("synthetic-{message}-{block}").as_bytes(),
            ));
        }
        texts.push(content);
    }
    let messages: Vec<_> = (0..128).map(|index| json!({"role":if index % 2 == 0 {"user"} else {"assistant"}, "content":texts[index % 64]})).collect();
    let mut report = json!({"synthetic":true, "samples_per_mode":samples, "preview_messages_per_request":128, "preview_bytes_per_message":4096,
        "unique_contents":64,"includes":"build_trace + compressed PostgreSQL archive + request/session/rollup/message transaction",
        "baseline_scope":"Legacy mode in this implementation, including pending NOTE creation; not the af2d800 binary",
        "modes":{}});
    for (label, action) in [
        ("legacy", Action::Legacy),
        ("dual_write", Action::DualWrite),
        ("references_only", Action::ReferencesOnly),
    ] {
        if label == "references_only" {
            loop {
                if message_migration::run(&pool, Action::Backfill, 1000, false).await?["affected_rows"]
                    == 0
                {
                    break;
                }
            }
        }
        message_migration::run(&pool, action, 1000, true).await?;
        let body = serde_json::to_vec(
            &json!({"model":"synthetic-model","metadata":{"session_id":format!("ingestion-{label}")},"messages":messages}),
        )?;
        let mut times = Vec::with_capacity(samples);
        for index in 0..samples + 5 {
            let mut event = TraceEvent::base(Uuid::new_v4(), Utc::now());
            event.original_uri = "/v1/chat/completions".into();
            event.upstream_url = "https://synthetic.invalid/v1/chat/completions".into();
            event.request_headers = json!({"content-type":"application/json"});
            event.request_body = body.clone();
            event.request_body_bytes = body.len() as i64;
            let start = Instant::now();
            let (trace, messages, uid, name) = build_trace(event, &plugins)?;
            anyhow::ensure!(
                messages.len() == 128,
                "benchmark must retain all preview occurrences"
            );
            storage::insert_trace(&pool, &archive, trace, messages, uid, name).await?;
            if index >= 5 {
                times.push(start.elapsed().as_secs_f64() * 1000.0);
            }
        }
        let sum_ms: f64 = times.iter().sum();
        report["modes"][label] = json!({"requests_per_second":samples as f64 / (sum_ms / 1000.0), "elapsed_ms":sum_ms,
            "p50_ms":super::percentile(&mut times,0.5),"p95_ms":super::percentile(&mut times,0.95),"p99_ms":super::percentile(&mut times,0.99)});
    }
    let legacy = report["modes"]["legacy"]["requests_per_second"]
        .as_f64()
        .ok_or_else(|| anyhow::anyhow!("missing ingestion baseline"))?;
    let references = report["modes"]["references_only"]["requests_per_second"]
        .as_f64()
        .ok_or_else(|| anyhow::anyhow!("missing ingestion result"))?;
    report["references_to_legacy_throughput_ratio"] = json!(references / legacy);
    let body = serde_json::to_vec(&json!({"messages":messages}))?;
    report["raw_compression"] = super::raw_compression::measure(&body)?;
    let path = std::env::var_os("LLMTRACE_INGEST_REPORT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("llmtrace-preview-ingestion.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(&report)?)?;
    println!("actual preview ingestion report: {}", path.display());
    println!("{}", serde_json::to_string(&report["modes"])?);
    Ok(())
}
