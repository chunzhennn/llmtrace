//! Cold PostgreSQL shared-buffer plans in a disposable, resource-limited server.
//! Never restarts the configured development/production PostgreSQL service.
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use crate::storage;
use crate::storage::query::ExplainPlanMode;

struct ColdDatabase {
    container: String,
}

impl Drop for ColdDatabase {
    fn drop(&mut self) {
        // Only the random, test-owned container and its anonymous volume.
        let _ = std::process::Command::new("docker")
            .args(["rm", "--force", "--volumes", &self.container])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

async fn docker(arguments: &[&str]) -> anyhow::Result<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new("docker")
            .args(arguments)
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    anyhow::ensure!(
        output.status.success(),
        "cold-cache fixture Docker command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

async fn connect(database: &ColdDatabase) -> anyhow::Result<PgPool> {
    // Docker may assign a different ephemeral host port after restart.
    let binding = docker(&["port", &database.container, "5432/tcp"]).await?;
    let port: u16 = binding
        .rsplit_once(':')
        .context("fixture port missing")?
        .1
        .parse()?;
    let url = format!("postgres://postgres@127.0.0.1:{port}/postgres");
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(pool) = PgPoolOptions::new()
                .max_connections(2)
                .acquire_timeout(Duration::from_secs(1))
                .connect(&url)
                .await
            {
                return Ok(pool);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await?
}

#[tokio::test]
#[ignore = "opt-in Docker cold PostgreSQL shared-buffer plans, disposable 2 GiB/4 CPU database"]
async fn performance_note_cold_cache_plans() -> anyhow::Result<()> {
    let database = ColdDatabase {
        container: format!("llmtrace-note-cold-{}", Uuid::new_v4().simple()),
    };
    docker(&[
        "run",
        "--detach",
        "--name",
        &database.container,
        "--label",
        "llmtrace.fixture=storage-query-cold",
        "--memory=2g",
        "--cpus=4",
        "--env",
        "POSTGRES_HOST_AUTH_METHOD=trust",
        "--publish",
        "127.0.0.1::5432",
        "postgres:16",
        "postgres",
        "-c",
        "autovacuum=off",
    ])
    .await?;
    let pool = connect(&database)
        .await
        .context("starting cold PostgreSQL fixture")?;
    storage::migrate(&pool).await?;
    let requests = super::request_count()?;
    super::seed_query_dataset(&pool, requests).await?;
    let version: String = sqlx::query_scalar("SELECT version()")
        .fetch_one(&pool)
        .await?;
    let size: i64 = sqlx::query_scalar("SELECT pg_database_size(current_database())::bigint")
        .fetch_one(&pool)
        .await?;
    pool.close().await;

    let mut plans = Vec::new();
    for (name, value) in [
        ("exact_common", "common"),
        ("exact_rare", "only_rare_🙂"),
        ("exact_absent", "not present"),
    ] {
        for (mode, label) in [
            (ExplainPlanMode::Custom, "force_custom_plan"),
            (ExplainPlanMode::Generic, "force_generic_plan"),
        ] {
            // There is no application, autovacuum or prewarm in this server.
            // Restart clears PG shared buffers; filesystem cache is uncontrolled.
            docker(&["restart", &database.container]).await?;
            let pool = connect(&database)
                .await
                .with_context(|| format!("reconnecting cold fixture for {name}/{label}"))?;
            let request = super::tag_query("eq", value)?;
            let cold = storage::query::explain(&pool, request.clone(), mode).await?;
            let warm = storage::query::explain(&pool, request, mode).await?;
            pool.close().await;
            plans.push(json!({"case":name, "plan_cache_mode":label,
                "first_after_postgres_restart":cold, "immediate_repeat":warm}));
        }
    }
    let image = docker(&["inspect", "--format", "{{.Image}}", &database.container]).await?;
    docker(&["rm", "--force", "--volumes", &database.container]).await?;
    let report: Value = json!({"synthetic":true, "postgres":version, "postgres_image_id":image,
        "memory_limit_bytes":2_u64 * 1024 * 1024 * 1024, "cpu_limit":4,
        "request_count":requests, "note_occurrences":requests, "session_count":requests / 10 + 1, "database_physical_bytes":size,
        "cache_scope":"first query after restarting only the disposable PostgreSQL process; PG shared buffers cold, OS page cache uncontrolled; single samples, not cold p95",
        "compiled_explain_analyze_buffers":plans, "owned_container_removed_on_exit":true});
    let path = std::env::var_os("LLMTRACE_COLD_REPORT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("llmtrace-note-cold.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(&report)?)?;
    drop(database);
    println!("cold PostgreSQL shared-buffer plans: {}", path.display());
    Ok(())
}
