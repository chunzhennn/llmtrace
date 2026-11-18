//! Opt-in P0/P1/P2 measurements on isolated synthetic PostgreSQL databases.
//! Reports physical sizes including indexes separately from logical text bytes.
use std::path::PathBuf;
use std::time::Instant;

use serde_json::{Value, json};
use sqlx::{PgPool, Row};

use crate::storage;
use crate::storage::StructuredQuery;
mod cold;
mod ingest;
mod raw_compression;
mod reads;

fn tag_query(op: &str, value: &str) -> anyhow::Result<StructuredQuery> {
    Ok(serde_json::from_value(
        json!({"dataset":"sessions", "filters":[
        {"field":"prompt_tag.name","op":"eq","value":"Bench"},
        {"field":"prompt_tag.value","op":op,"value":value}
    ], "limit":100}),
    )?)
}

async fn distribution(pool: &PgPool, unique: i64, occurrences: i64) -> anyhow::Result<Value> {
    sqlx::raw_sql("TRUNCATE session_messages, message_contents RESTART IDENTITY CASCADE;
        DROP TABLE IF EXISTS optimization_legacy_messages;
        CREATE TABLE optimization_legacy_messages(id bigserial PRIMARY KEY, request_id uuid NOT NULL, session_id uuid NOT NULL,
            role text NOT NULL, content text NOT NULL, created_at timestamptz NOT NULL, content_truncated boolean);
        CREATE INDEX ON optimization_legacy_messages(request_id);
        CREATE INDEX ON optimization_legacy_messages(session_id, created_at, id);
        CREATE INDEX ON optimization_legacy_messages(created_at DESC, id DESC);")
        .execute(pool).await?;
    // Use a permanent synthetic helper: pooled SQL statements need not use the
    // connection that created a temporary table.
    sqlx::raw_sql("DROP TABLE IF EXISTS optimization_texts; CREATE TABLE optimization_texts(key bigint PRIMARY KEY, content text NOT NULL)").execute(pool).await?;
    sqlx::query("INSERT INTO optimization_texts SELECT i, (SELECT string_agg(md5(i::text || ':' || block::text), '' ORDER BY block) FROM generate_series(1,128) block) FROM generate_series(0,$1 - 1) i")
        .bind(unique).execute(pool).await?;
    let request_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM request_traces LIMIT 1")
        .fetch_one(pool)
        .await?;
    let session_id: uuid::Uuid =
        sqlx::query_scalar("SELECT session_id FROM request_traces WHERE id = $1")
            .bind(request_id)
            .fetch_one(pool)
            .await?;
    let started = Instant::now();
    sqlx::query("INSERT INTO optimization_legacy_messages(request_id, session_id, role, content, created_at, content_truncated)
        SELECT $1, $2, CASE WHEN i % 2 = 0 THEN 'user' ELSE 'assistant' END, t.content, '2026-11-01T00:00:00Z'::timestamptz, false
        FROM generate_series(1,$3) i JOIN optimization_texts t ON t.key = i % $4")
        .bind(request_id).bind(session_id).bind(occurrences).bind(unique).execute(pool).await?;
    let legacy_insert_ms = started.elapsed().as_secs_f64() * 1000.0;
    let started = Instant::now();
    sqlx::query("INSERT INTO message_contents(content_sha256, content_bytes, content) SELECT sha256(convert_to(content, 'UTF8')), octet_length(content), content FROM optimization_texts ORDER BY key")
        .execute(pool).await?;
    sqlx::query("INSERT INTO session_messages(request_id, session_id, role, content_id, created_at, content_truncated)
        SELECT $1, $2, CASE WHEN i % 2 = 0 THEN 'user' ELSE 'assistant' END, c.id, '2026-11-01T00:00:00Z'::timestamptz, false
        FROM generate_series(1,$3) i JOIN optimization_texts t ON t.key = i % $4
        JOIN message_contents c ON c.content_sha256 = sha256(convert_to(t.content, 'UTF8'))")
        .bind(request_id).bind(session_id).bind(occurrences).bind(unique).execute(pool).await?;
    let normalized_insert_ms = started.elapsed().as_secs_f64() * 1000.0;
    let row = sqlx::query("SELECT pg_total_relation_size('optimization_legacy_messages')::bigint AS legacy_bytes,
        (pg_total_relation_size('session_messages') + pg_total_relation_size('message_contents'))::bigint AS normalized_bytes,
        (SELECT COUNT(*) FROM trace_messages WHERE content IS NULL)::bigint AS missing_contents")
        .fetch_one(pool).await?;
    let legacy_bytes: i64 = row.try_get("legacy_bytes")?;
    let normalized_bytes: i64 = row.try_get("normalized_bytes")?;
    anyhow::ensure!(
        row.try_get::<i64, _>("missing_contents")? == 0,
        "normalized benchmark lost preview content"
    );
    sqlx::query("DROP TABLE optimization_texts")
        .execute(pool)
        .await?;
    Ok(
        json!({"occurrences":occurrences, "unique_contents":unique, "preview_bytes":occurrences * 4096,
        "unique_preview_bytes":unique * 4096, "legacy_physical_bytes":legacy_bytes, "normalized_physical_bytes":normalized_bytes,
        "net_saved_fraction":1.0 - normalized_bytes as f64 / legacy_bytes as f64,
        "bulk_legacy_insert_ms":legacy_insert_ms, "bulk_normalized_insert_ms":normalized_insert_ms,
        "bulk_sql_is_not_application_ingestion_throughput":true}),
    )
}

fn percentile(values: &mut [f64], fraction: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    values[((values.len() - 1) as f64 * fraction).ceil() as usize]
}

async fn timings(pool: &PgPool, request: StructuredQuery) -> anyhow::Result<Value> {
    let mut values = Vec::new();
    let mut rows = 0;
    for _ in 0..30 {
        let start = Instant::now();
        let result = storage::run_structured_query(pool, request.clone()).await?;
        rows = result["rows"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("invalid benchmark query rows"))?
            .len();
        values.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    Ok(
        json!({"samples":values.len(), "rows":rows, "p50_ms":percentile(&mut values,0.5), "p95_ms":percentile(&mut values,0.95), "p99_ms":percentile(&mut values,0.99)}),
    )
}

fn request_count() -> anyhow::Result<i64> {
    let requests = std::env::var("LLMTRACE_OPTIMIZATION_REQUESTS")
        .ok()
        .map(|value| value.parse::<i64>())
        .transpose()?
        .unwrap_or(1_000_000);
    anyhow::ensure!(
        (1000..=2_000_000).contains(&requests),
        "benchmark request count must be 1000..=2000000"
    );
    Ok(requests)
}

async fn seed_query_dataset(pool: &PgPool, requests: i64) -> anyhow::Result<()> {
    let tags = storage::note_index::configure_test_tags(pool, &["Bench"]).await?;
    sqlx::query("INSERT INTO trace_sessions(id, session_key, first_seen, last_seen, user_name)
        SELECT md5('session-' || i)::uuid, 'synthetic-session-' || i, '2026-11-01T00:00:00Z'::timestamptz,
            '2026-11-01T00:00:00Z'::timestamptz + make_interval(secs => i), 'synthetic user' FROM generate_series(0, $1 / 10) i")
        .bind(requests).execute(pool).await?;
    sqlx::query("INSERT INTO request_traces(id, session_id, started_at, method, original_uri, upstream_url, model, request_kind)
        SELECT md5('request-' || i)::uuid, md5('session-' || (i / 10))::uuid, '2026-11-01T00:00:00Z'::timestamptz + make_interval(secs => i),
            'POST', '/v1/chat/completions', 'https://synthetic.invalid', 'synthetic-model', 'openai_chat_completions' FROM generate_series(1, $1) i")
        .bind(requests).execute(pool).await?;
    sqlx::query("INSERT INTO request_prompt_index_state(request_id, index_version, state, capture_complete, tag_revision) SELECT id, 1, 'ready', true, $1 FROM request_traces").bind(tags.revision).execute(pool).await?;
    sqlx::query("INSERT INTO note_values(id, index_version, value_sha256, value, value_bytes)
        SELECT i, 1, sha256(convert_to(value, 'UTF8')), value, octet_length(value) FROM
        (SELECT i, CASE WHEN i = 1 THEN 'common' WHEN i = 10002 THEN 'only_rare_🙂' WHEN i = 10003 THEN '' ELSE 'cohort-' || i END AS value FROM generate_series(1,10003) i) synthetic_values")
        .execute(pool).await?;
    sqlx::query("INSERT INTO request_notes(id, request_id, index_version, field_ordinal, match_ordinal, source_path, source_kind, role, note_value_id, decoded_start_byte, decoded_end_byte, tag_name)
        SELECT md5('note-' || i)::uuid, md5('request-' || i)::uuid, 1, 0, 0, '/messages/0/content', 'message', 'user',
        CASE WHEN i = 1 THEN 10002 WHEN i % 2 = 0 THEN 1 ELSE 2 + i % 10000 END, 0, 16, 'Bench' FROM generate_series(1,$1) i")
        .bind(requests).execute(pool).await?;
    sqlx::raw_sql("ANALYZE trace_sessions; ANALYZE request_traces; ANALYZE request_prompt_index_state; ANALYZE request_notes; ANALYZE note_values;").execute(pool).await?;
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "opt-in million-request storage/query benchmark; requires local PostgreSQL"]
async fn performance_storage_query_optimization(pool: PgPool) -> anyhow::Result<()> {
    let requests = request_count()?;
    seed_query_dataset(&pool, requests).await?;
    let version: String = sqlx::query_scalar("SELECT version()")
        .fetch_one(&pool)
        .await?;
    let high_repeat = distribution(&pool, 100, 20_000).await?;
    let low_repeat = distribution(&pool, 20_000, 20_000).await?;
    let mut report = json!({"synthetic":true,"postgres":version,"request_count":requests,"note_occurrences":requests,
        "session_count":requests / 10 + 1,"preview_cases":{"high_repetition":high_repeat,"no_repetition":low_repeat},
        "cache_scope":"warm repetitions after ANALYZE; OS cold cache not measured", "queries":{}});
    for (name, op, value) in [
        ("exact_rare", "eq", "only_rare_🙂"),
        ("exact_common", "eq", "common"),
        ("exact_absent", "eq", "not present"),
        ("contains_rare", "contains", "rare_🙂"),
        ("contains_short", "contains", "o"),
        ("contains_symbol", "contains", "_"),
        ("contains_common", "contains", "cohort"),
    ] {
        report["queries"][name] = timings(&pool, tag_query(op, value)?).await?;
    }
    report["concurrent_queries_and_ingestion"] = concurrent_load(&pool).await?;
    for (name, value) in [("exact_rare", "only_rare_🙂"), ("exact_common", "common")] {
        for (mode, mode_name) in [
            (storage::query::ExplainPlanMode::Custom, "custom"),
            (storage::query::ExplainPlanMode::Generic, "generic"),
        ] {
            report[format!("{name}_{mode_name}_compiled_explain")] =
                storage::query::explain(&pool, tag_query("eq", value)?, mode).await?;
        }
    }
    let path = std::env::var_os("LLMTRACE_OPTIMIZATION_REPORT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("llmtrace-storage-query-optimization.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(&report)?)?;
    println!("storage/query optimization report: {}", path.display());
    println!("{}", serde_json::to_string(&report["queries"])?);
    Ok(())
}

async fn concurrent_load(pool: &PgPool) -> anyhow::Result<Value> {
    use crate::config::{ArchiveConfig, ArchiveStorageBackend};
    use crate::metrics::RuntimeMetrics;
    use crate::plugins::PluginManager;
    use crate::trace::{TraceEvent, build_trace};
    use std::sync::Arc;
    let barrier = Arc::new(tokio::sync::Barrier::new(9));
    let executor = Arc::new(storage::QueryExecutor::new(pool));
    let mut readers = tokio::task::JoinSet::new();
    for worker in 0..8 {
        let executor = executor.clone();
        let barrier = barrier.clone();
        let value = if worker % 2 == 0 {
            "common"
        } else {
            "only_rare_🙂"
        };
        let request = tag_query("eq", value)?;
        readers.spawn(async move {
            barrier.wait().await;
            let mut times = Vec::new();
            for _ in 0..10 {
                let start = Instant::now();
                let result = executor.run(request.clone()).await?;
                anyhow::ensure!(
                    !result["rows"]
                        .as_array()
                        .ok_or_else(|| anyhow::anyhow!("missing concurrent rows"))?
                        .is_empty(),
                    "lost a known positive NOTE"
                );
                times.push(start.elapsed().as_secs_f64() * 1000.0);
            }
            Ok::<_, anyhow::Error>(times)
        });
    }
    let write_pool = pool.clone();
    let write_barrier = barrier.clone();
    let writer = tokio::spawn(async move {
        let plugins = PluginManager::load(&[], &Default::default(), &RuntimeMetrics::default())?;
        let archive = ArchiveConfig {
            storage_backend: ArchiveStorageBackend::Postgres,
            ..Default::default()
        };
        let mut times = Vec::new();
        write_barrier.wait().await;
        for _ in 0..80 {
            let start = Instant::now();
            let mut event = TraceEvent::base(uuid::Uuid::new_v4(), chrono::Utc::now());
            event.original_uri = "/v1/chat/completions".into();
            event.upstream_url = "https://synthetic.invalid/v1/chat/completions".into();
            event.request_headers = json!({"content-type":"application/json"});
            event.request_body = br#"{"metadata":{"session_id":"concurrent-collector"},"messages":[{"role":"user","content":"synthetic concurrent collection"}]}"#.to_vec();
            event.request_body_bytes = event.request_body.len() as i64;
            let (trace, messages, uid, name) = build_trace(event, &plugins)?;
            storage::insert_trace(&write_pool, &archive, trace, messages, uid, name).await?;
            times.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        Ok::<_, anyhow::Error>(times)
    });
    let mut read_times = Vec::new();
    while let Some(result) = readers.join_next().await {
        read_times.extend(result??);
    }
    let mut write_times = writer.await??;
    storage::readiness_check(pool).await?;
    Ok(
        json!({"concurrent_query_clients":8,"query_samples":read_times.len(),"ingested_requests":write_times.len(),"ingest_failures":0,
        "query_p50_ms":percentile(&mut read_times,0.5),"query_p95_ms":percentile(&mut read_times,0.95),"query_p99_ms":percentile(&mut read_times,0.99),
        "ingest_p95_ms":percentile(&mut write_times,0.95),"readiness_passed":true,
        "scope":"separate four-connection query pool and application ingestion transaction; proxy/journal measured separately"}),
    )
}
