//! Real query HTTP traffic, proxy capture and NOTE indexing in the same window.
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{Json, Router, routing::post};
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::sync::Barrier;

use crate::config::ArchiveStorageBackend;

use super::{Server, metric, percentile};

const SAMPLES: usize = 200;

#[sqlx::test(migrations = "./migrations")]
#[ignore = "opt-in combined HTTP query/proxy/index load; build the release binary first"]
async fn performance_note_queries_during_proxy_capture(pool: PgPool) -> anyhow::Result<()> {
    crate::storage::note_index::configure_test_tags(&pool, &["Bench"]).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let upstream = format!("http://{}", listener.local_addr()?);
    let mock = tokio::spawn(async move {
        axum::serve(listener, Router::new().fallback(post(|| async {
            Json(json!({"choices":[{"message":{"role":"assistant","content":"synthetic response"}}]}))
        }))).await
    });
    let server = Server::start(&pool, &upstream, ArchiveStorageBackend::Postgres, 0, false).await?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let cookie = server.login(&client).await?;
    let body = json!({"model":"synthetic-model", "messages":[{"role":"user","content":format!("{} [Bench: simultaneous-load]", "synthetic ".repeat(100))}]});
    client
        .post(format!("{}/v1/chat/completions", server.base))
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    server.settle(&client).await?;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let indexed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_notes")
                .fetch_one(&pool)
                .await?;
            if indexed == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        Ok::<_, sqlx::Error>(())
    })
    .await??;

    let before = server.metrics(&client).await?;
    // Eight readers, one writer and one monitor start at the same barrier.
    // Own all futures until completion, including on an individual failure.
    let barrier = Arc::new(Barrier::new(10));
    let readers = stream::iter(0..8).map(|_| {
        let barrier = barrier.clone();
        let client = &client;
        let server = &server;
        let cookie = &cookie;
        async move {
            let mut tick = tokio::time::interval(Duration::from_millis(25));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut times = Vec::with_capacity(SAMPLES);
            barrier.wait().await;
            for _ in 0..SAMPLES {
                tick.tick().await;
                let started = Instant::now();
                let result: Value = client.post(format!("{}/api/query", server.base))
                    .header("cookie", cookie).header("origin", &server.base)
                    .json(&json!({"dataset":"sessions", "filters":[{"field":"prompt_tag.name","op":"eq","value":"Bench"},{"field":"prompt_tag.value","op":"eq","value":"simultaneous-load"}], "limit":100}))
                    .send().await?.error_for_status()?.json().await?;
                anyhow::ensure!(result["rows"].as_array().is_some_and(|rows| !rows.is_empty()), "lost known NOTE match during capture");
                times.push(started.elapsed().as_secs_f64() * 1000.0);
            }
            Ok::<_, anyhow::Error>(times)
        }
    }).buffer_unordered(8).collect::<Vec<_>>();
    let writer = async {
        let mut tick = tokio::time::interval(Duration::from_millis(25));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut times = Vec::with_capacity(SAMPLES);
        barrier.wait().await;
        let started = Instant::now();
        for _ in 0..SAMPLES {
            tick.tick().await;
            let request_started = Instant::now();
            client
                .post(format!("{}/v1/chat/completions", server.base))
                .json(&body)
                .send()
                .await?
                .error_for_status()?
                .bytes()
                .await?;
            times.push(request_started.elapsed().as_secs_f64() * 1000.0);
        }
        Ok::<_, anyhow::Error>((times, started.elapsed().as_secs_f64()))
    };
    let monitor = async {
        let mut tick = tokio::time::interval(Duration::from_millis(50));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut samples = Vec::new();
        barrier.wait().await;
        for _ in 0..SAMPLES / 2 {
            tick.tick().await;
            client
                .get(format!("{}/readyz", server.base))
                .send()
                .await?
                .error_for_status()?;
            let metrics = server.metrics(&client).await?;
            let lock_waiters: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock'").fetch_one(&pool).await?;
            samples.push(json!({"journal_pending_records":metrics.get("llmtrace_journal_pending_records").copied().unwrap_or_default(),
                "queue_depth":metric(&metrics,"queue_depth"), "database_lock_waiters":lock_waiters}));
        }
        Ok::<_, anyhow::Error>(samples)
    };
    let (readers, writer, monitoring) = tokio::join!(readers, writer, monitor);
    let read_times = readers
        .into_iter()
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let (write_times, input_seconds) = writer?;
    let monitoring = monitoring?;
    let at_input_end = server.metrics(&client).await?;
    let drain_seconds = server.settle(&client).await?;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let remaining: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM request_prompt_index_state WHERE state <> 'ready'",
            )
            .fetch_one(&pool)
            .await?;
            if remaining == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        Ok::<_, sqlx::Error>(())
    })
    .await??;
    let after = server.metrics(&client).await?;
    for name in [
        "dropped_full_total",
        "dropped_memory_total",
        "dropped_closed_total",
        "persist_failures_total",
        "build_failures_total",
    ] {
        anyhow::ensure!(
            metric(&after, name) == metric(&before, name),
            "combined load dropped or failed capture: {name}"
        );
    }
    for name in [
        "llmtrace_journal_dropped_full_total",
        "llmtrace_journal_write_failed_total",
        "llmtrace_journal_read_failed_total",
        "llmtrace_journal_ack_failed_total",
        "llmtrace_note_index_failures_total",
    ] {
        anyhow::ensure!(
            after
                .get(name)
                .zip(before.get(name))
                .is_some_and(|(after, before)| after == before),
            "combined load failed durable capture: {name}"
        );
    }
    anyhow::ensure!(
        metric(&after, "persisted_total") - metric(&before, "persisted_total") == SAMPLES as f64,
        "combined load did not persist every request"
    );
    let indexed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_notes")
        .fetch_one(&pool)
        .await?;
    anyhow::ensure!(
        indexed == (SAMPLES + 1) as i64,
        "combined load NOTE indexing incomplete"
    );
    let freshness: Value = sqlx::query_scalar("SELECT jsonb_build_object('samples', COUNT(*), 'p50_p95_p99_ms',
        percentile_cont(ARRAY[0.5,0.95,0.99]) WITHIN GROUP (ORDER BY EXTRACT(EPOCH FROM (i.indexed_at - r.completed_at)) * 1000))
        FROM request_prompt_index_state i JOIN request_traces r ON r.id = i.request_id WHERE i.indexed_at IS NOT NULL").fetch_one(&pool).await?;
    let report = json!({"synthetic":true, "concurrent_http_query_clients":8, "query_samples":read_times.len(), "captured_http_requests":SAMPLES,
        "input_seconds":input_seconds, "input_requests_per_second":SAMPLES as f64 / input_seconds,
        "query_p50_p95_p99_ms":[percentile(&read_times,0.5),percentile(&read_times,0.95),percentile(&read_times,0.99)],
        "proxy_p50_p95_p99_ms":[percentile(&write_times,0.5),percentile(&write_times,0.95),percentile(&write_times,0.99)],
        "monitoring":monitoring, "readiness_checks_passed":SAMPLES / 2, "capture_failures":0, "query_failures":0,
        "pipeline_after_inputs":at_input_end, "pipeline_after_drain":after, "drain_seconds":drain_seconds, "index_freshness":freshness,
        "scope":"finite simultaneous HTTP load, real proxy/journal/worker, separate server read pool; not a long-duration capacity or OS cold-cache result"});
    let path = std::env::var_os("LLMTRACE_NOTE_LOAD_REPORT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("llmtrace-note-load.json"));
    fs::write(&path, serde_json::to_vec_pretty(&report)?)?;
    let root = server.root.clone();
    drop(server);
    fs::remove_dir_all(root)?;
    mock.abort();
    let _ = mock.await;
    println!("combined NOTE query/proxy load report: {}", path.display());
    Ok(())
}
