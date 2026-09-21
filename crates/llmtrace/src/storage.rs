//! Storage facade and shared transaction setup. SQL, archive I/O, ingestion and
//! read projections live in the corresponding `storage/` modules.
use std::future::Future;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use std::{fs, io};

use anyhow::Context;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder, Row, Transaction};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::config::{ArchiveConfig, ArchiveStorageBackend, StorageConfig};
use crate::metrics::RuntimeMetrics;
use crate::types::{ParsedMessage, RequestKind};

mod analytics;
mod archive;
mod audit;
mod bodies;
mod ingest;
mod models;
mod paging;
mod query;
mod requests;
mod retention;
mod rotation;
pub mod session_export;
mod sessions;

pub use analytics::{
    api_key_usage, data_integrity, data_overview, error_summary, latency_summary, model_usage,
    stats, upstream_health, usage_summary, usage_timeseries, user_usage,
};
pub use archive::decompress_with_limit;
pub use audit::{
    audit_summary, list_audit_events, list_ui_sessions, record_ui_audit_event, revoke_ui_session,
};
pub use ingest::{TraceRecord, insert_trace};
pub(crate) use ingest::{insert_journaled_trace, journal_trace_persisted};
pub use query::{
    StructuredQuery, StructuredQueryError, run_structured_query, structured_query_schema,
};
pub use requests::{
    RequestListFilters, get_request, list_requests, list_session_requests, recent_error_requests,
    request_facets, slow_requests,
};
pub use retention::{
    RetentionPruneResult, retention_status, spawn_retention_pruner, storage_summary,
};
pub use rotation::spawn_archive_maintenance;
pub use sessions::{get_session, list_sessions};

use archive::*;
use models::*;
use paging::*;
use retention::*;

const STATUS_CLASS_SQL: &str = r#"CASE
                   WHEN status IS NULL THEN 'no_status'
                   WHEN status BETWEEN 100 AND 199 THEN '1xx'
                   WHEN status BETWEEN 200 AND 299 THEN '2xx'
                   WHEN status BETWEEN 300 AND 399 THEN '3xx'
                   WHEN status BETWEEN 400 AND 499 THEN '4xx'
                   WHEN status BETWEEN 500 AND 599 THEN '5xx'
                   ELSE 'other'
               END"#;
const DATA_INTEGRITY_MISMATCH_LIMIT: i64 = 50;
const API_READ_STATEMENT_TIMEOUT_SQL: &str = "SET LOCAL statement_timeout = '5s'";
const READINESS_CHECK_TIMEOUT: Duration = Duration::from_secs(2);
pub async fn connect(config: &StorageConfig) -> anyhow::Result<PgPool> {
    Ok(PgPoolOptions::new()
        .max_connections(config.max_connections)
        .acquire_timeout(Duration::from_secs(config.acquire_timeout_secs))
        .connect(&config.postgres_url)
        .await?)
}

pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}

pub async fn readiness_check(pool: &PgPool) -> anyhow::Result<()> {
    readiness_check_with_timeout(
        async {
            sqlx::query_scalar::<_, i32>("SELECT 1")
                .fetch_one(pool)
                .await?;
            Ok(())
        },
        READINESS_CHECK_TIMEOUT,
    )
    .await
}

async fn readiness_check_with_timeout<F>(
    future: F,
    timeout_duration: Duration,
) -> anyhow::Result<()>
where
    F: Future<Output = anyhow::Result<()>>,
{
    match tokio::time::timeout(timeout_duration, future).await {
        Ok(result) => result,
        Err(_) => anyhow::bail!(
            "readiness check timed out after {} ms",
            timeout_duration.as_millis()
        ),
    }
}

async fn begin_api_read_tx(pool: &PgPool) -> anyhow::Result<Transaction<'_, Postgres>> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION READ ONLY")
        .execute(&mut *tx)
        .await?;
    sqlx::query(API_READ_STATEMENT_TIMEOUT_SQL)
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

async fn begin_api_snapshot_tx(pool: &PgPool) -> anyhow::Result<Transaction<'static, Postgres>> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    sqlx::query(API_READ_STATEMENT_TIMEOUT_SQL)
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

fn nonnegative_i64(value: i64) -> i64 {
    value.max(0)
}

fn saturating_add_i64(left: i64, right: i64) -> i64 {
    left.saturating_add(nonnegative_i64(right))
}

fn seconds_since(now: DateTime<Utc>, value: Option<DateTime<Utc>>) -> Option<i64> {
    value.map(|value| now.signed_duration_since(value).num_seconds().max(0))
}

fn seconds_until(now: DateTime<Utc>, value: DateTime<Utc>) -> i64 {
    value.signed_duration_since(now).num_seconds().max(0)
}

fn rate(numerator: i64, denominator: i64) -> f64 {
    if denominator <= 0 {
        0.0
    } else {
        numerator.max(0) as f64 / denominator as f64
    }
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn read_rows<T: for<'row> FromRow<'row, sqlx::postgres::PgRow>>(
    rows: Vec<sqlx::postgres::PgRow>,
) -> Result<Vec<T>, sqlx::Error> {
    rows.iter().map(T::from_row).collect()
}

#[cfg(test)]
mod integration;
#[cfg(test)]
mod openrouter;
#[cfg(test)]
mod performance;
#[cfg(test)]
mod read_tests;
#[cfg(test)]
mod routing_integration;
#[cfg(test)]
mod tests;
