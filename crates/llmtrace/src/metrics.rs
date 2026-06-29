use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

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
}
