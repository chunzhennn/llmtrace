//! Owns the prompt tag scheduler, every async/blocking task and bounded shutdown.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};

use crate::config::ArchiveConfig;
use crate::metrics::RuntimeMetrics;
use crate::prompt_tags;
use crate::prompt_tags::{
    ExtractionLimits, NoteDiagnostic, NoteIndexVersion, RequestPromptMetadata, TagExtraction,
};

use super::note_index_store;
use super::note_index_store::{ClaimedNoteJob, LoadedPrompt, PublishOutcome};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct NoteIndexOptions {
    #[cfg(test)]
    #[serde(skip)]
    test_hooks: Option<Arc<WorkerTestHooks>>,
    pub(crate) enabled: bool,
    pub(crate) concurrency: usize,
    pub(crate) max_connections: u32,
    pub(crate) backfill_batch_size: i64,
    pub(crate) backfill_interval_secs: u64,
    pub(crate) build_version: Option<NoteIndexVersion>,
    pub(crate) poll_interval_ms: u64,
    pub(crate) lease_secs: i64,
    pub(crate) shutdown_timeout_secs: u64,
    pub(crate) max_attempts: i32,
    pub(crate) max_body_bytes: usize,
    pub(crate) max_segment_bytes: usize,
    pub(crate) max_scan_bytes: usize,
    pub(crate) max_fields: usize,
    pub(crate) max_value_bytes: usize,
    pub(crate) max_notes: usize,
    pub(crate) max_total_value_bytes: usize,
}

impl Default for NoteIndexOptions {
    fn default() -> Self {
        let limits = ExtractionLimits::default();
        Self {
            #[cfg(test)]
            test_hooks: None,
            enabled: true,
            concurrency: 1,
            max_connections: 2,
            backfill_batch_size: 100,
            backfill_interval_secs: 30,
            build_version: None,
            poll_interval_ms: 1000,
            lease_secs: 60,
            shutdown_timeout_secs: 10,
            max_attempts: 3,
            max_body_bytes: limits.max_body_bytes(),
            max_segment_bytes: 64 * 1024 * 1024,
            max_scan_bytes: limits.max_scan_bytes(),
            max_fields: limits.max_fields(),
            max_value_bytes: limits.max_value_bytes(),
            max_notes: limits.max_notes(),
            max_total_value_bytes: limits.max_total_value_bytes(),
        }
    }
}

#[cfg(test)]
#[derive(Debug)]
struct WorkerTestHooks {
    before_parse: std::sync::Barrier,
    release_parse: std::sync::Barrier,
}

impl NoteIndexOptions {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        self.extraction_limits()?;
        for (name, value, maximum) in [
            ("concurrency", self.concurrency, 4),
            ("max_connections", self.max_connections as usize, 8),
            (
                "max_segment_bytes",
                self.max_segment_bytes,
                256 * 1024 * 1024,
            ),
        ] {
            anyhow::ensure!(
                (1..=maximum).contains(&value),
                "note_index.{name} must be 1..={maximum}"
            );
        }
        anyhow::ensure!(
            self.max_connections as usize > self.concurrency,
            "note_index.max_connections must exceed concurrency"
        );
        anyhow::ensure!(
            self.max_segment_bytes >= self.max_body_bytes,
            "prompt tag segment budget is smaller than its body budget"
        );
        anyhow::ensure!(
            (1..=1000).contains(&self.backfill_batch_size),
            "invalid prompt tag backfill batch size"
        );
        anyhow::ensure!(
            (1..=3600).contains(&self.backfill_interval_secs),
            "invalid prompt tag backfill interval"
        );
        anyhow::ensure!(
            (100..=60_000).contains(&self.poll_interval_ms),
            "invalid prompt tag polling interval"
        );
        anyhow::ensure!(
            (30..=600).contains(&self.lease_secs),
            "invalid prompt tag lease duration"
        );
        anyhow::ensure!(
            (1..=60).contains(&self.shutdown_timeout_secs),
            "invalid prompt tag shutdown timeout"
        );
        anyhow::ensure!(
            (1..=10).contains(&self.max_attempts),
            "invalid prompt tag retry limit"
        );
        Ok(())
    }
    pub(crate) fn extraction_limits(&self) -> anyhow::Result<ExtractionLimits> {
        ExtractionLimits::try_new(
            self.max_body_bytes,
            self.max_scan_bytes,
            self.max_fields,
            self.max_value_bytes,
            self.max_notes,
            self.max_total_value_bytes,
        )
    }
}

#[derive(Default, Serialize)]
pub(crate) struct NoteIndexReport {
    completed_tasks: u64,
    failed_tasks: u64,
}

/// On timeout this result owns the still-running scheduler. It must be joined
/// with `finish` before closing the database; no blocking task is detached.
#[must_use]
#[derive(Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum NoteIndexShutdown {
    Finished {
        report: NoteIndexReport,
    },
    InProgress {
        #[serde(skip)]
        handle: JoinHandle<NoteIndexReport>,
    },
}

impl NoteIndexShutdown {
    pub(crate) async fn finish(self) -> anyhow::Result<NoteIndexReport> {
        match self {
            Self::Finished { report } => Ok(report),
            Self::InProgress { handle } => Ok(handle.await?),
        }
    }
}

pub(crate) struct NoteIndexWorker {
    shutdown: watch::Sender<bool>,
    stop: Arc<AtomicBool>,
    handle: JoinHandle<NoteIndexReport>,
    shutdown_timeout: Duration,
    metrics: RuntimeMetrics,
}

struct WorkerHealthGuard(RuntimeMetrics);
impl Drop for WorkerHealthGuard {
    fn drop(&mut self) {
        self.0.note_index_running(false);
    }
}

impl NoteIndexWorker {
    /// Takes ownership of a separate, small pool. It must not be the ingestion
    /// pool clone. Invalid options fail before starting any task.
    pub(crate) fn try_new(
        pool: PgPool,
        archive: ArchiveConfig,
        options: NoteIndexOptions,
        metrics: RuntimeMetrics,
    ) -> anyhow::Result<Self> {
        options.validate()?;
        let (shutdown, receiver) = watch::channel(false);
        let stop = Arc::new(AtomicBool::new(false));
        let shutdown_timeout = Duration::from_secs(options.shutdown_timeout_secs);
        let handle = tokio::spawn(run(
            pool,
            archive,
            options,
            metrics.clone(),
            receiver,
            stop.clone(),
        ));
        Ok(Self {
            shutdown,
            stop,
            handle,
            shutdown_timeout,
            metrics,
        })
    }

    /// Stops claims, asks blocking scans to stop, then joins owned work. If the
    /// budget expires, no uncommitted job is acknowledged; leases recover later.
    pub(crate) async fn shutdown(mut self) -> anyhow::Result<NoteIndexShutdown> {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.shutdown.send(true);
        match tokio::time::timeout(self.shutdown_timeout, &mut self.handle).await {
            Ok(result) => Ok(NoteIndexShutdown::Finished { report: result? }),
            Err(_) => {
                self.metrics.note_index_failed();
                tracing::warn!(
                    "prompt tag shutdown budget exhausted; returning owned unfinished work for explicit joining"
                );
                Ok(NoteIndexShutdown::InProgress {
                    handle: self.handle,
                })
            }
        }
    }
}

async fn run(
    pool: PgPool,
    archive: ArchiveConfig,
    options: NoteIndexOptions,
    metrics: RuntimeMetrics,
    mut shutdown: watch::Receiver<bool>,
    stop: Arc<AtomicBool>,
) -> NoteIndexReport {
    metrics.note_index_running(true);
    let _health = WorkerHealthGuard(metrics.clone());
    let mut tasks = JoinSet::new();
    let mut report = NoteIndexReport::default();
    let mut tick = tokio::time::interval(Duration::from_millis(options.poll_interval_ms));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut active_version = None;
    let mut next_coverage = tokio::time::Instant::now();
    let mut next_backfill = tokio::time::Instant::now();
    loop {
        tokio::select! {
            biased;
            _ = shutdown.changed() => break,
            result = tasks.join_next(), if !tasks.is_empty() => observe_task(result, &mut report, &metrics),
            _ = tick.tick() => {
                let (active, enabled, backfill) = match note_index_store::worker_settings(&pool).await {
                    Ok(settings) => settings,
                    Err(error) => {
                        metrics.note_index_failed();
                        tracing::warn!(%error, "prompt tag settings unavailable");
                        continue;
                    }
                };
                let version = options.build_version.unwrap_or(active);
                active_version = enabled.then_some(version);
                let now = tokio::time::Instant::now();
                if enabled && now >= next_coverage {
                    if let Err(error) = note_index_store::refresh_coverage(&pool, version).await {
                        metrics.note_index_failed(); tracing::warn!(%error, "prompt tag coverage summary deferred");
                    }
                    next_coverage = now + Duration::from_secs(30);
                }
                if now >= next_backfill {
                    if backfill && let Err(error) = note_index_store::enqueue_missing(&pool, version, options.backfill_batch_size).await {
                        metrics.note_index_failed(); tracing::warn!(%error, "prompt tag backfill deferred");
                    }
                    if let Err(error) = note_index_store::collect_unused(&pool, options.backfill_batch_size).await {
                        metrics.note_index_failed(); tracing::warn!(%error, "prompt tag dictionary collection deferred");
                    }
                    next_backfill = now + Duration::from_secs(options.backfill_interval_secs);
                }
            }
        }
        if let Some(version) = active_version {
            while tasks.len() < options.concurrency && !stop.load(Ordering::Relaxed) {
                match note_index_store::claim(
                    &pool,
                    version,
                    options.lease_secs,
                    options.max_attempts,
                )
                .await
                {
                    Ok(Some(job)) => {
                        tasks.spawn(process(
                            pool.clone(),
                            archive.clone(),
                            options.clone(),
                            job,
                            stop.clone(),
                        ));
                    }
                    Ok(None) => break,
                    Err(error) => {
                        metrics.note_index_failed();
                        tracing::warn!(%error, "prompt tag claim deferred");
                        break;
                    }
                }
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    while let Some(result) = tasks.join_next().await {
        observe_task(Some(result), &mut report, &metrics);
    }
    pool.close().await;
    report
}

fn observe_task(
    result: Option<Result<anyhow::Result<PublishOutcome>, tokio::task::JoinError>>,
    report: &mut NoteIndexReport,
    metrics: &RuntimeMetrics,
) {
    match result {
        Some(Ok(Ok(PublishOutcome::Published))) => {
            report.completed_tasks += 1;
            metrics.note_index_published();
        }
        Some(Ok(Ok(PublishOutcome::LeaseLost | PublishOutcome::SourceRemoved))) => {}
        Some(Ok(Err(error))) => {
            report.failed_tasks += 1;
            metrics.note_index_failed();
            tracing::warn!(%error, "prompt tag task failed; durable lease is recoverable");
        }
        Some(Err(error)) => {
            report.failed_tasks += 1;
            metrics.note_index_failed();
            tracing::error!(%error, "prompt tag task exited unexpectedly");
        }
        None => {}
    }
}

async fn process(
    pool: PgPool,
    archive: ArchiveConfig,
    options: NoteIndexOptions,
    mut job: ClaimedNoteJob,
    stop: Arc<AtomicBool>,
) -> anyhow::Result<PublishOutcome> {
    let source = match note_index_store::load_source(
        &pool,
        &archive,
        &job,
        options.max_body_bytes,
        options.max_segment_bytes,
    )
    .await
    {
        Ok(Some(source)) => source,
        Ok(None) => return Ok(PublishOutcome::SourceRemoved),
        Err(error) => {
            let request_id = job.request_id();
            note_index_store::retry(&pool, job, options.max_attempts).await?;
            tracing::warn!(%request_id, %error, "prompt tag source deferred");
            return Err(error.into());
        }
    };
    let LoadedPrompt {
        metadata,
        body,
        unavailable,
    } = source;
    let result = if let Some(result) = unavailable {
        if result
            .diagnostics
            .contains(&NoteDiagnostic::UnreadableSource)
        {
            note_index_store::retry(&pool, job, options.max_attempts).await?;
            anyhow::bail!("prompt tag source unreadable; bounded retry scheduled");
        }
        result
    } else if let Some(body) = body {
        let limits = options.extraction_limits()?;
        let tags = std::mem::take(&mut job.tags);
        let task_stop = stop.clone();
        #[cfg(test)]
        let hooks = options.test_hooks.clone();
        tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            if let Some(hooks) = hooks {
                hooks.before_parse.wait();
                hooks.release_parse.wait();
            }
            prompt_tags::extract(
                &body,
                RequestPromptMetadata {
                    request_kind: &metadata.summary.request_kind,
                    original_uri: &metadata.summary.original_uri,
                    request_headers: &metadata.request_headers,
                    capture_complete: !metadata.summary.request_body_truncated,
                },
                &tags,
                limits,
                &task_stop,
            )
        })
        .await?
    } else {
        TagExtraction::unavailable(
            NoteDiagnostic::MissingSource,
            !metadata.summary.request_body_truncated,
        )
    };
    if stop.load(Ordering::Relaxed) {
        return Ok(PublishOutcome::LeaseLost);
    }
    Ok(note_index_store::publish(&pool, job, result).await?)
}

#[cfg(test)]
mod tests;
