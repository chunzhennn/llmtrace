use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::storage::RetentionPruneResult;

#[derive(Debug, Clone, Default)]
pub struct RuntimeMetrics {
    inner: Arc<RuntimeMetricsInner>,
}

#[derive(Debug, Default)]
struct RuntimeMetricsInner {
    traces_enqueued: AtomicU64,
    journal_written: AtomicU64,
    journal_dropped_full: AtomicU64,
    journal_write_failed: AtomicU64,
    journal_read_failed: AtomicU64,
    journal_ack_failed: AtomicU64,
    journal_retry: AtomicU64,
    journal_recovered: AtomicU64,

    traces_dropped_full: AtomicU64,
    traces_dropped_memory: AtomicU64,
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TraceQueueMetrics {
    pub capacity: u64,
    pub available: u64,
    pub depth: u64,
    pub memory_limit_bytes: u64,
    pub memory_used_bytes: u64,
    pub journal: crate::trace::journal::JournalSnapshot,
}

impl TraceQueueMetrics {
    pub fn new(capacity: usize, available: usize) -> Self {
        let available = available.min(capacity);
        Self {
            capacity: usize_to_u64(capacity),
            available: usize_to_u64(available),
            depth: usize_to_u64(capacity.saturating_sub(available)),
            ..Default::default()
        }
    }
}

impl RuntimeMetrics {
    pub fn journal_written(&self) {
        self.inner.journal_written.fetch_add(1, Ordering::Relaxed);
    }
    pub fn journal_dropped_full(&self) {
        let dropped = self
            .inner
            .journal_dropped_full
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        if dropped.is_power_of_two() {
            tracing::warn!(dropped, "capture journal is full; new captures rejected");
        }
    }
    pub fn journal_write_failed(&self) -> bool {
        self.inner
            .journal_write_failed
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
            .is_power_of_two()
    }
    pub fn journal_read_failed(&self) -> bool {
        self.inner
            .journal_read_failed
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
            .is_power_of_two()
    }
    pub fn journal_ack_failed(&self) {
        self.inner
            .journal_ack_failed
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn journal_retry(&self) -> bool {
        self.inner
            .journal_retry
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
            .is_power_of_two()
    }
    pub fn journal_recovered(&self) {
        self.inner.journal_recovered.fetch_add(1, Ordering::Relaxed);
    }

    pub fn trace_dropped_memory(&self) {
        self.inner
            .traces_dropped_memory
            .fetch_add(1, Ordering::Relaxed);
    }
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

    pub fn snapshot(&self, trace_queue: TraceQueueMetrics) -> Value {
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
                "dropped_memory": self.inner.traces_dropped_memory.load(Ordering::Relaxed),
                "dropped_closed": self.inner.traces_dropped_closed.load(Ordering::Relaxed),
                "build_failures": self.inner.trace_build_failures.load(Ordering::Relaxed),
                "persist_failures": self.inner.trace_persist_failures.load(Ordering::Relaxed),
                "queue_capacity": trace_queue.capacity,
                "queue_available": trace_queue.available,
                "queue_depth": trace_queue.depth,
                "memory_limit_bytes": trace_queue.memory_limit_bytes,
                "memory_used_bytes": trace_queue.memory_used_bytes,
            },
            "trace_journal": {
                "enabled": trace_queue.journal.enabled,
                "acknowledgement": "background_fsync_after_response",
                "not_yet_durable": if trace_queue.journal.enabled { self.inner.traces_enqueued.load(Ordering::Relaxed)
                    .saturating_sub(self.inner.journal_written.load(Ordering::Relaxed))
                    .saturating_sub(self.inner.journal_dropped_full.load(Ordering::Relaxed))
                    .saturating_sub(self.inner.journal_write_failed.load(Ordering::Relaxed)) } else { 0 },
                "pending_records": trace_queue.journal.pending_records,
                "pending_bytes": trace_queue.journal.pending_bytes,
                "max_bytes": trace_queue.journal.max_bytes,
                "max_records": trace_queue.journal.max_records,
                "quarantined_records": trace_queue.journal.quarantined_records,
                "quarantined_bytes": trace_queue.journal.quarantined_bytes,
                "oldest_pending_age_secs": trace_queue.journal.oldest_pending_age_secs,
                "blocked_records": trace_queue.journal.blocked_records,
                "written": self.inner.journal_written.load(Ordering::Relaxed),
                "dropped_full": self.inner.journal_dropped_full.load(Ordering::Relaxed),
                "write_failed": self.inner.journal_write_failed.load(Ordering::Relaxed),
                "read_failed": self.inner.journal_read_failed.load(Ordering::Relaxed),
                "ack_failed": self.inner.journal_ack_failed.load(Ordering::Relaxed),
                "retry": self.inner.journal_retry.load(Ordering::Relaxed),
                "recovered": self.inner.journal_recovered.load(Ordering::Relaxed),
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

    pub fn prometheus_text(
        &self,
        db_pool_size: u32,
        db_pool_idle: usize,
        trace_queue: TraceQueueMetrics,
    ) -> String {
        let counters = self.counters();
        let retention = self.retention_snapshot();
        let mut output = String::new();
        push_metric(
            &mut output,
            "counter",
            "llmtrace_journal_written_total",
            "Process lifetime journal written count.",
            self.inner.journal_written.load(Ordering::Relaxed),
        );
        push_metric(
            &mut output,
            "counter",
            "llmtrace_journal_dropped_full_total",
            "Process lifetime journal dropped full count.",
            self.inner.journal_dropped_full.load(Ordering::Relaxed),
        );
        push_metric(
            &mut output,
            "counter",
            "llmtrace_journal_write_failed_total",
            "Process lifetime journal write failed count.",
            self.inner.journal_write_failed.load(Ordering::Relaxed),
        );
        push_metric(
            &mut output,
            "counter",
            "llmtrace_journal_read_failed_total",
            "Process lifetime journal read failed count.",
            self.inner.journal_read_failed.load(Ordering::Relaxed),
        );
        push_metric(
            &mut output,
            "counter",
            "llmtrace_journal_ack_failed_total",
            "Process lifetime journal ack failed count.",
            self.inner.journal_ack_failed.load(Ordering::Relaxed),
        );
        push_metric(
            &mut output,
            "counter",
            "llmtrace_journal_retry_total",
            "Process lifetime journal retry count.",
            self.inner.journal_retry.load(Ordering::Relaxed),
        );
        push_metric(
            &mut output,
            "counter",
            "llmtrace_journal_recovered_total",
            "Process lifetime journal recovered count.",
            self.inner.journal_recovered.load(Ordering::Relaxed),
        );
        push_metric(
            &mut output,
            "gauge",
            "llmtrace_journal_pending_records",
            "Capture journal pending records.",
            trace_queue.journal.pending_records,
        );
        push_metric(
            &mut output,
            "gauge",
            "llmtrace_journal_pending_bytes",
            "Capture journal pending bytes.",
            trace_queue.journal.pending_bytes,
        );
        push_metric(
            &mut output,
            "gauge",
            "llmtrace_journal_max_bytes",
            "Capture journal max bytes.",
            trace_queue.journal.max_bytes,
        );
        push_metric(
            &mut output,
            "gauge",
            "llmtrace_journal_max_records",
            "Capture journal max records.",
            trace_queue.journal.max_records,
        );
        push_metric(
            &mut output,
            "gauge",
            "llmtrace_journal_quarantined_records",
            "Capture journal quarantined records.",
            trace_queue.journal.quarantined_records,
        );
        push_metric(
            &mut output,
            "gauge",
            "llmtrace_journal_quarantined_bytes",
            "Capture journal quarantined bytes.",
            trace_queue.journal.quarantined_bytes,
        );
        push_metric(
            &mut output,
            "gauge",
            "llmtrace_journal_oldest_pending_age_secs",
            "Capture journal oldest pending age secs.",
            trace_queue.journal.oldest_pending_age_secs,
        );
        push_metric(
            &mut output,
            "gauge",
            "llmtrace_journal_blocked_records",
            "Capture journal blocked records.",
            trace_queue.journal.blocked_records,
        );
        push_metric(
            &mut output,
            "gauge",
            "llmtrace_journal_enabled",
            "Whether asynchronous durable capture is enabled.",
            u64::from(trace_queue.journal.enabled),
        );

        push_metric(
            &mut output,
            "counter",
            "llmtrace_trace_pipeline_dropped_memory_total",
            "Trace events dropped because the memory budget was full.",
            self.inner.traces_dropped_memory.load(Ordering::Relaxed),
        );
        push_metric(
            &mut output,
            "gauge",
            "llmtrace_trace_pipeline_memory_limit_bytes",
            "Memory budget for captured events queued or being processed.",
            trace_queue.memory_limit_bytes,
        );
        push_metric(
            &mut output,
            "gauge",
            "llmtrace_trace_pipeline_memory_used_bytes",
            "Estimated captured event allocations queued or being processed; excludes processing scratch space and live captures.",
            trace_queue.memory_used_bytes,
        );

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
            "gauge",
            "llmtrace_trace_pipeline_queue_capacity",
            "Configured capacity of the bounded trace pipeline queue.",
            trace_queue.capacity,
        );
        push_metric(
            &mut output,
            "gauge",
            "llmtrace_trace_pipeline_queue_available",
            "Currently available slots in the bounded trace pipeline queue.",
            trace_queue.available,
        );
        push_metric(
            &mut output,
            "gauge",
            "llmtrace_trace_pipeline_queue_depth",
            "Current number of events waiting in the bounded trace pipeline queue.",
            trace_queue.depth,
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

fn usize_to_u64(value: usize) -> u64 {
    value.try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_queue_metrics_reports_depth_from_available_capacity() {
        assert_eq!(
            TraceQueueMetrics::new(10, 7),
            TraceQueueMetrics {
                capacity: 10,
                available: 7,
                depth: 3,
                ..Default::default()
            }
        );
    }

    #[test]
    fn trace_queue_metrics_clamps_available_to_capacity() {
        assert_eq!(
            TraceQueueMetrics::new(4, 9),
            TraceQueueMetrics {
                capacity: 4,
                available: 4,
                depth: 0,
                ..Default::default()
            }
        );
    }

    #[test]
    fn runtime_metrics_snapshot_counts_trace_events() {
        let metrics = RuntimeMetrics::default();

        metrics.trace_enqueued();
        metrics.trace_dropped_full();
        metrics.trace_dropped_memory();
        metrics.trace_dropped_closed();
        metrics.trace_build_failed();
        metrics.trace_persist_failed();
        metrics.trace_persisted();

        let snapshot = metrics.snapshot(TraceQueueMetrics::new(10, 7));
        assert_eq!(snapshot["trace_pipeline"]["enqueued"], 1);
        assert_eq!(snapshot["trace_pipeline"]["dropped_full"], 1);
        assert_eq!(snapshot["trace_pipeline"]["dropped_memory"], 1);
        assert_eq!(snapshot["trace_pipeline"]["dropped_closed"], 1);
        assert_eq!(snapshot["trace_pipeline"]["build_failures"], 1);
        assert_eq!(snapshot["trace_pipeline"]["persist_failures"], 1);
        assert_eq!(snapshot["trace_pipeline"]["persisted"], 1);
        assert_eq!(snapshot["trace_pipeline"]["queue_capacity"], 10);
        assert_eq!(snapshot["trace_pipeline"]["queue_available"], 7);
        assert_eq!(snapshot["trace_pipeline"]["queue_depth"], 3);
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

        let snapshot = metrics.snapshot(TraceQueueMetrics::default());
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

        let text = metrics.prometheus_text(7, 3, TraceQueueMetrics::new(11, 8));

        assert!(text.contains("# TYPE llmtrace_trace_pipeline_enqueued_total counter"));
        assert!(text.contains("llmtrace_trace_pipeline_enqueued_total 1"));
        assert!(text.contains("llmtrace_trace_pipeline_dropped_full_total 1"));
        assert!(text.contains("llmtrace_trace_pipeline_queue_capacity 11"));
        assert!(text.contains("llmtrace_trace_pipeline_queue_available 8"));
        assert!(text.contains("llmtrace_trace_pipeline_queue_depth 3"));
        assert!(text.contains("llmtrace_retention_last_deleted_total 21"));
        assert!(text.contains("llmtrace_db_pool_size 7"));
        assert!(text.contains("llmtrace_db_pool_idle 3"));
        assert!(!text.contains("boom"));
    }
}
