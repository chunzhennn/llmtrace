use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::fmt::Write;

use crate::storage::RetentionPruneResult;

#[derive(Debug, Clone, Default)]
pub struct RuntimeMetrics {
    inner: Arc<RuntimeMetricsInner>,
}

#[derive(Debug, Default)]
struct RuntimeMetricsInner {
    traces_enqueued: AtomicU64,
    traces_dropped_full: AtomicU64,
    traces_dropped_closed: AtomicU64,
    trace_build_failures: AtomicU64,
    trace_persist_failures: AtomicU64,
    traces_persisted: AtomicU64,
    retention: Mutex<RetentionMetrics>,
}

#[derive(Debug, Default, Clone)]
struct RetentionMetrics {
    runs: u64,
    failures: u64,
    last_success_at: Option<DateTime<Utc>>,
    last_failure_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
    last_deleted: RetentionPruneResult,
}

impl RuntimeMetrics {
    pub fn trace_enqueued(&self) {
        self.inner.traces_enqueued.fetch_add(1, Ordering::Relaxed);
    }

    pub fn trace_dropped_full(&self) {
        self.inner
            .traces_dropped_full
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn trace_dropped_closed(&self) {
        self.inner
            .traces_dropped_closed
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn trace_build_failed(&self) {
        self.inner
            .trace_build_failures
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn trace_persist_failed(&self) {
        self.inner
            .trace_persist_failures
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn trace_persisted(&self) {
        self.inner.traces_persisted.fetch_add(1, Ordering::Relaxed);
    }

    pub fn retention_succeeded(&self, result: &RetentionPruneResult) {
        let mut retention = self
            .inner
            .retention
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        retention.runs = retention.runs.saturating_add(1);
        retention.last_success_at = Some(Utc::now());
        retention.last_deleted = result.clone();
    }

    pub fn retention_failed(&self, error: String) {
        let mut retention = self
            .inner
            .retention
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        retention.failures = retention.failures.saturating_add(1);
        retention.last_failure_at = Some(Utc::now());
        retention.last_error = Some(error);
    }

    pub fn snapshot(&self) -> Value {
        let retention = self
            .inner
            .retention
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();

        json!({
            "trace_pipeline": {
                "enqueued": self.inner.traces_enqueued.load(Ordering::Relaxed),
                "persisted": self.inner.traces_persisted.load(Ordering::Relaxed),
                "dropped_full": self.inner.traces_dropped_full.load(Ordering::Relaxed),
                "dropped_closed": self.inner.traces_dropped_closed.load(Ordering::Relaxed),
                "build_failures": self.inner.trace_build_failures.load(Ordering::Relaxed),
                "persist_failures": self.inner.trace_persist_failures.load(Ordering::Relaxed),
            },
            "retention": {
                "runs": retention.runs,
                "failures": retention.failures,
                "last_success_at": retention.last_success_at,
                "last_failure_at": retention.last_failure_at,
                "last_error": retention.last_error,
                "last_deleted": {
                    "request_traces": retention.last_deleted.request_traces,
                    "trace_rollups_minute": retention.last_deleted.trace_rollups_minute,
                    "trace_sessions": retention.last_deleted.trace_sessions,
                    "ui_audit_events": retention.last_deleted.ui_audit_events,
                    "ui_sessions": retention.last_deleted.ui_sessions,
                    "oauth_states": retention.last_deleted.oauth_states,
                    "total": retention.last_deleted.total_deleted(),
                },
            },
        })
    }

    pub fn prometheus_text(&self, db_pool_size: u32, db_pool_idle: usize) -> String {
        let counters = self.counters();
        let retention = self.retention_snapshot();
        let mut output = String::new();

        push_metric(
            &mut output,
            "counter",
            "llmtrace_trace_pipeline_enqueued_total",
            "Trace events accepted by the bounded background queue.",
            counters.traces_enqueued,
        );
        push_metric(
            &mut output,
            "counter",
            "llmtrace_trace_pipeline_persisted_total",
            "Trace events successfully persisted.",
            counters.traces_persisted,
        );
        push_metric(
            &mut output,
            "counter",
            "llmtrace_trace_pipeline_dropped_full_total",
            "Trace events dropped because the bounded background queue was full.",
            counters.traces_dropped_full,
        );
        push_metric(
            &mut output,
            "counter",
            "llmtrace_trace_pipeline_dropped_closed_total",
            "Trace events dropped because the background queue was closed.",
            counters.traces_dropped_closed,
        );
        push_metric(
            &mut output,
            "counter",
            "llmtrace_trace_pipeline_build_failures_total",
            "Trace events that failed during parse, redaction, compression, or plugin processing.",
            counters.trace_build_failures,
        );
        push_metric(
            &mut output,
            "counter",
            "llmtrace_trace_pipeline_persist_failures_total",
            "Trace events that failed during database persistence.",
            counters.trace_persist_failures,
        );
        push_metric(
            &mut output,
            "counter",
            "llmtrace_retention_runs_total",
            "Successful retention prune runs.",
            retention.runs,
        );
        push_metric(
            &mut output,
            "counter",
            "llmtrace_retention_failures_total",
            "Failed retention prune runs.",
            retention.failures,
        );
        push_metric(
            &mut output,
            "gauge",
            "llmtrace_retention_last_success_timestamp_seconds",
            "Unix timestamp of the most recent successful retention prune run, or 0 if none.",
            timestamp_seconds(retention.last_success_at),
        );
        push_metric(
            &mut output,
            "gauge",
            "llmtrace_retention_last_failure_timestamp_seconds",
            "Unix timestamp of the most recent failed retention prune run, or 0 if none.",
            timestamp_seconds(retention.last_failure_at),
        );
        push_metric(
            &mut output,
            "gauge",
            "llmtrace_retention_last_deleted_total",
            "Rows deleted by the most recent successful retention prune run.",
            retention.last_deleted.total_deleted(),
        );
        push_metric(
            &mut output,
            "gauge",
            "llmtrace_retention_last_deleted_request_traces",
            "Request trace rows deleted by the most recent successful retention prune run.",
            retention.last_deleted.request_traces,
        );
        push_metric(
            &mut output,
            "gauge",
            "llmtrace_retention_last_deleted_trace_rollups_minute",
            "Minute rollup rows deleted by the most recent successful retention prune run.",
            retention.last_deleted.trace_rollups_minute,
        );
        push_metric(
            &mut output,
            "gauge",
            "llmtrace_retention_last_deleted_trace_sessions",
            "Trace session rows deleted by the most recent successful retention prune run.",
            retention.last_deleted.trace_sessions,
        );
        push_metric(
            &mut output,
            "gauge",
            "llmtrace_retention_last_deleted_ui_audit_events",
            "UI audit event rows deleted by the most recent successful retention prune run.",
            retention.last_deleted.ui_audit_events,
        );
        push_metric(
            &mut output,
            "gauge",
            "llmtrace_retention_last_deleted_ui_sessions",
            "Expired UI session rows deleted by the most recent successful retention prune run.",
            retention.last_deleted.ui_sessions,
        );
        push_metric(
            &mut output,
            "gauge",
            "llmtrace_retention_last_deleted_oauth_states",
            "Expired OAuth state rows deleted by the most recent successful retention prune run.",
            retention.last_deleted.oauth_states,
        );
        push_metric(
            &mut output,
            "gauge",
            "llmtrace_db_pool_size",
            "Current number of database connections managed by the pool.",
            db_pool_size as u64,
        );
        push_metric(
            &mut output,
            "gauge",
            "llmtrace_db_pool_idle",
            "Current number of idle database connections in the pool.",
            db_pool_idle as u64,
        );

        output
    }

    fn counters(&self) -> RuntimeCounters {
        RuntimeCounters {
            traces_enqueued: self.inner.traces_enqueued.load(Ordering::Relaxed),
            traces_dropped_full: self.inner.traces_dropped_full.load(Ordering::Relaxed),
            traces_dropped_closed: self.inner.traces_dropped_closed.load(Ordering::Relaxed),
            trace_build_failures: self.inner.trace_build_failures.load(Ordering::Relaxed),
            trace_persist_failures: self.inner.trace_persist_failures.load(Ordering::Relaxed),
            traces_persisted: self.inner.traces_persisted.load(Ordering::Relaxed),
        }
    }

    fn retention_snapshot(&self) -> RetentionMetrics {
        self.inner
            .retention
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

#[derive(Debug, Clone, Copy)]
struct RuntimeCounters {
    traces_enqueued: u64,
    traces_dropped_full: u64,
    traces_dropped_closed: u64,
    trace_build_failures: u64,
    trace_persist_failures: u64,
    traces_persisted: u64,
}

fn push_metric(output: &mut String, kind: &str, name: &str, help: &str, value: u64) {
    let _ = writeln!(output, "# HELP {name} {help}");
    let _ = writeln!(output, "# TYPE {name} {kind}");
    let _ = writeln!(output, "{name} {value}");
}

fn timestamp_seconds(timestamp: Option<DateTime<Utc>>) -> u64 {
    timestamp
        .and_then(|timestamp| timestamp.timestamp().try_into().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_metrics_snapshot_counts_trace_events() {
        let metrics = RuntimeMetrics::default();

        metrics.trace_enqueued();
        metrics.trace_dropped_full();
        metrics.trace_dropped_closed();
        metrics.trace_build_failed();
        metrics.trace_persist_failed();
        metrics.trace_persisted();

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot["trace_pipeline"]["enqueued"], 1);
        assert_eq!(snapshot["trace_pipeline"]["dropped_full"], 1);
        assert_eq!(snapshot["trace_pipeline"]["dropped_closed"], 1);
        assert_eq!(snapshot["trace_pipeline"]["build_failures"], 1);
        assert_eq!(snapshot["trace_pipeline"]["persist_failures"], 1);
        assert_eq!(snapshot["trace_pipeline"]["persisted"], 1);
    }

    #[test]
    fn runtime_metrics_snapshot_reports_retention_status() {
        let metrics = RuntimeMetrics::default();
        metrics.retention_succeeded(&RetentionPruneResult {
            request_traces: 1,
            trace_rollups_minute: 2,
            trace_sessions: 3,
            ui_audit_events: 4,
            ui_sessions: 5,
            oauth_states: 6,
        });
        metrics.retention_failed("boom".to_string());

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot["retention"]["runs"], 1);
        assert_eq!(snapshot["retention"]["failures"], 1);
        assert_eq!(snapshot["retention"]["last_error"], "boom");
        assert_eq!(snapshot["retention"]["last_deleted"]["total"], 21);
    }

    #[test]
    fn prometheus_text_exports_safe_runtime_metrics() {
        let metrics = RuntimeMetrics::default();
        metrics.trace_enqueued();
        metrics.trace_dropped_full();
        metrics.retention_succeeded(&RetentionPruneResult {
            request_traces: 1,
            trace_rollups_minute: 2,
            trace_sessions: 3,
            ui_audit_events: 4,
            ui_sessions: 5,
            oauth_states: 6,
        });

        let text = metrics.prometheus_text(7, 3);

        assert!(text.contains("# TYPE llmtrace_trace_pipeline_enqueued_total counter"));
        assert!(text.contains("llmtrace_trace_pipeline_enqueued_total 1"));
        assert!(text.contains("llmtrace_trace_pipeline_dropped_full_total 1"));
        assert!(text.contains("llmtrace_retention_last_deleted_total 21"));
        assert!(text.contains("llmtrace_db_pool_size 7"));
        assert!(text.contains("llmtrace_db_pool_idle 3"));
        assert!(!text.contains("boom"));
    }
}
