use super::{
    BuiltTrace, QueuedTrace, build_priced_trace,
    journal::{Claim, Full, Journal},
    memory::MemoryBudget,
};
use crate::{
    config::ArchiveConfig, metrics::RuntimeMetrics, plugins::PluginManager, pricing::PriceTable,
    storage,
};
use sqlx::PgPool;
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
};
use uuid::Uuid;

#[derive(Clone)]
pub(super) struct Context {
    pub pool: PgPool,
    pub plugins: Arc<PluginManager>,
    pub archive: ArchiveConfig,
    pub pricing: Arc<PriceTable>,
    pub memory: Arc<MemoryBudget>,
    pub metrics: RuntimeMetrics,
    pub journal: Arc<Journal>,
}

pub(super) async fn run(
    context: Context,
    mut receiver: mpsc::Receiver<QueuedTrace>,
    worker_count: usize,
) {
    let (finished, done) = watch::channel(false);
    let mut services = JoinSet::new();
    services.spawn(replay(context.clone(), done.clone(), worker_count));
    services.spawn(clean_receipts(context.clone(), done));
    while let Some(queued) = receiver.recv().await {
        let journal = context.journal.clone();
        let event = queued.event;
        let id = event.id;
        let result = tokio::task::spawn_blocking(move || journal.append(&event)).await;
        // Database work cannot retain intake memory or stall the journal writer.
        drop(queued.reservation);
        match result {
            Ok(Ok(())) => context.metrics.journal_written(),
            Ok(Err(error)) if error.is::<Full>() => {
                context.metrics.journal_dropped_full();
            }
            error => {
                if context.metrics.journal_write_failed() {
                    tracing::error!(trace_id=%id, ?error, "capture was not durably journaled");
                }
            }
        }
    }
    let _ = finished.send(true);
    while let Some(result) = services.join_next().await {
        if let Err(error) = result {
            tracing::error!(%error, "journal service stopped unexpectedly");
        }
    }
}

async fn replay(context: Context, mut done: watch::Receiver<bool>, worker_count: usize) {
    let mut workers = JoinSet::new();
    let mut tick = tokio::time::interval(Duration::from_secs(
        context.journal.config.retry_interval_secs,
    ));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        while workers.len() < worker_count {
            let Some(claim) = context.journal.claim(&context.memory) else {
                break;
            };
            workers.spawn(process(context.clone(), claim));
        }
        if *done.borrow()
            && context
                .journal
                .snapshot(context.memory.limit)
                .pending_records
                == 0
            && workers.is_empty()
        {
            break;
        }
        tokio::select! {
            result = workers.join_next(), if !workers.is_empty() => {
                if let Some(Err(error)) = result { tracing::error!(%error, "journal replay worker stopped unexpectedly"); }
            }
            _ = context.journal.notify.notified() => (),
            _ = tick.tick() => (),
            _ = done.changed(), if !*done.borrow() => (),
        }
    }
}

async fn process(context: Context, claim: Claim) {
    let result = process_inner(&context, &claim).await;
    if let Err(error) = result
        && context.metrics.journal_retry()
    {
        tracing::warn!(trace_id=%claim.id, %error, "journal record retained for retry");
    }
    // The claim requeues an unacknowledged record even if this task is cancelled.
}

async fn process_inner(context: &Context, claim: &Claim) -> anyhow::Result<()> {
    let already_persisted =
        storage::journal_trace_persisted(&context.pool, claim.id, context.journal.id).await?;
    if !already_persisted {
        let journal = context.journal.clone();
        let id = claim.id;
        let event = match tokio::task::spawn_blocking(move || journal.read(id)).await? {
            Ok(event) => event,
            Err(error) => {
                let report = context.metrics.journal_read_failed();
                // Temporary I/O failures retain the original record for retry.
                if error.downcast_ref::<std::io::Error>().is_some() {
                    return Err(error);
                }
                let journal = context.journal.clone();
                tokio::task::spawn_blocking(move || journal.quarantine(id)).await??;
                if report {
                    tracing::error!(trace_id=%id, %error, "invalid journal record quarantined");
                }
                return Ok(());
            }
        };
        let built: BuiltTrace = build_priced_trace(
            event,
            context.plugins.clone(),
            &context.pricing,
            &context.metrics,
        )
        .await?;
        if let Err(error) = storage::insert_journaled_trace(
            &context.pool,
            &context.archive,
            built.0,
            built.1,
            built.2,
            built.3,
            context.journal.id,
        )
        .await
        {
            context.metrics.trace_persist_failed();
            return Err(error);
        }
    }
    let journal = context.journal.clone();
    let id = claim.id;
    let acknowledged = tokio::task::spawn_blocking(move || journal.acknowledge(id)).await?;
    if let Err(error) = acknowledged {
        context.metrics.journal_ack_failed();
        return Err(error);
    }
    context.metrics.trace_persisted();
    if claim.recovered {
        context.metrics.journal_recovered();
    }
    Ok(())
}

async fn clean_receipts(context: Context, mut done: watch::Receiver<bool>) {
    let mut cursor = Uuid::nil();
    let mut tick = tokio::time::interval(Duration::from_secs(
        context.journal.config.retry_interval_secs,
    ));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! { _ = done.changed() => break, _ = tick.tick() => () }
        if *done.borrow() {
            break;
        }
        match clean_receipt_page(&context, cursor).await {
            Ok(next) => cursor = next,
            Err(error) => tracing::warn!(%error, "journal receipt cleanup will retry"),
        }
    }
}

async fn clean_receipt_page(context: &Context, cursor: Uuid) -> anyhow::Result<Uuid> {
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT trace_id FROM trace_ingest_receipts WHERE journal_id=$1 AND trace_id>$2 ORDER BY trace_id LIMIT 4096")
        .bind(context.journal.id).bind(cursor).fetch_all(&context.pool).await?;
    let next = if ids.len() == 4096 {
        *ids.last().unwrap()
    } else {
        Uuid::nil()
    };
    let removable: Vec<_> = ids
        .into_iter()
        .filter(|id| context.journal.receipt_can_be_removed(*id))
        .collect();
    if !removable.is_empty() {
        sqlx::query("DELETE FROM trace_ingest_receipts WHERE journal_id=$1 AND trace_id=ANY($2)")
            .bind(context.journal.id)
            .bind(removable)
            .execute(&context.pool)
            .await?;
    }
    Ok(next)
}
