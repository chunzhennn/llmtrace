use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sqlx::PgPool;
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};
use uuid::Uuid;

use crate::metrics::{RuntimeMetrics, TraceQueueMetrics};
use crate::parsers;
use crate::plugins::{HookInput, PluginEffects, PluginManager};
use crate::redaction;
use crate::storage::{self, ParsedMessage, TraceRecord};
use crate::types::{PluginHook, RequestKind};

mod durable;
pub(crate) mod journal;
mod memory;
use memory::{MemoryBudget, Reservation, event_bytes};

const MAX_SESSION_MESSAGES_PER_TRACE: usize = 128;
const MAX_SESSION_MESSAGE_ROLE_BYTES: usize = 64;
const MAX_SESSION_MESSAGE_CONTENT_BYTES: usize = 16 * 1024;
const MAX_TRACE_SESSION_KEY_BYTES: usize = 1024;
const MAX_TRACE_IDENTITY_BYTES: usize = 1024;
const MAX_TRACE_TAGS: usize = 64;
const MAX_TRACE_TAG_BYTES: usize = 128;
const SESSION_MESSAGES_TRUNCATED_TAG: &str = "session_messages_truncated";
const TRACE_ENRICHMENT_TRUNCATED_TAG: &str = "trace_enrichment_truncated";

#[derive(Debug, Serialize, Deserialize)]
pub struct TraceEvent {
    pub id: Uuid,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub method: String,
    pub original_uri: String,
    pub upstream_url: String,
    pub upstream_host: Option<String>,
    pub status: Option<i32>,
    pub error: Option<String>,
    pub request_kind: Option<RequestKind>,
    pub model: Option<String>,
    pub api_key_hash: Option<String>,
    pub session_key: Option<String>,
    pub ttft_ms: Option<i64>,
    pub ttfb_ms: Option<i64>,
    pub response_chunk_timings: Vec<(usize, i64)>,
    pub duration_ms: Option<i64>,
    pub request_body_bytes: i64,
    pub response_body_bytes: i64,
    pub request_headers: Value,
    pub response_headers: Value,
    pub plugin_request_headers: Value,
    pub plugin_response_headers: Value,
    #[serde(skip)]
    pub request_body: Vec<u8>,
    #[serde(skip)]
    pub response_body: Vec<u8>,
    pub request_body_truncated: bool,
    pub response_body_truncated: bool,
    pub content_type: Option<String>,
    pub plugin_metadata: Value,
    pub tags: Vec<String>,
    pub messages: Vec<ParsedMessage>,
    pub user_id: Option<String>,
    pub user_name: Option<String>,
    pub run_plugins: bool,
}

impl TraceEvent {
    /// An empty event keyed by id/start time; callers override the fields they set.
    pub fn base(id: Uuid, started_at: DateTime<Utc>) -> Self {
        Self {
            id,
            started_at,
            completed_at: None,
            method: String::new(),
            original_uri: String::new(),
            upstream_url: String::new(),
            upstream_host: None,
            status: None,
            error: None,
            request_kind: None,
            model: None,
            api_key_hash: None,
            session_key: None,
            ttft_ms: None,
            ttfb_ms: None,
            response_chunk_timings: Vec::new(),
            duration_ms: None,
            request_body_bytes: 0,
            response_body_bytes: 0,
            request_headers: json!({}),
            response_headers: json!({}),
            plugin_request_headers: json!({}),
            plugin_response_headers: json!({}),
            request_body: Vec::new(),
            response_body: Vec::new(),
            request_body_truncated: false,
            response_body_truncated: false,
            content_type: None,
            plugin_metadata: json!({}),
            tags: Vec::new(),
            messages: Vec::new(),
            user_id: None,
            user_name: None,
            run_plugins: false,
        }
    }
}

type BuiltTrace = (
    TraceRecord,
    Vec<ParsedMessage>,
    Option<String>,
    Option<String>,
);

#[derive(Clone)]
pub struct TraceRecorder {
    sender: mpsc::Sender<QueuedTrace>,
    queue_capacity: usize,
    memory: Arc<MemoryBudget>,
    dropped: Arc<AtomicU64>,
    metrics: RuntimeMetrics,
    journal: Option<Arc<journal::Journal>>,
}

#[derive(Debug)]
struct QueuedTrace {
    event: TraceEvent,
    reservation: Reservation,
}

/// Owns the background dispatcher task so it can be drained on shutdown.
pub struct TracePipeline {
    handle: JoinHandle<()>,
}

impl TracePipeline {
    /// Waits for in-flight traces to finish once all recorders have been dropped.
    pub async fn drain(mut self) {
        if tokio::time::timeout(Duration::from_secs(10), &mut self.handle)
            .await
            .is_err()
        {
            tracing::warn!("trace pipeline drain timed out; journaled records remain for replay");
            self.handle.abort();
            let _ = (&mut self.handle).await;
        }
    }
}

impl Drop for TracePipeline {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl TraceRecorder {
    pub fn spawn(
        pool: PgPool,
        plugins: Arc<PluginManager>,
        archive: crate::config::ArchiveConfig,
        pricing: Arc<crate::pricing::PriceTable>,
        config: &crate::config::StorageConfig,
        metrics: RuntimeMetrics,
    ) -> anyhow::Result<(Self, TracePipeline)> {
        let queue_capacity = config.trace_queue_capacity.max(1);
        let worker_count = config.trace_worker_count.max(1);
        let memory = MemoryBudget::new(config.trace_queue_max_bytes);
        let (sender, receiver) = mpsc::channel(queue_capacity);

        let journal = if config.journal.enabled {
            Some(journal::Journal::open(
                &config.journal,
                &config.postgres_url,
            )?)
        } else {
            None
        };
        tracing::info!(
            queue_capacity,
            worker_count,
            journal_enabled = journal.is_some(),
            "trace pipeline started"
        );
        let handle = if let Some(journal) = &journal {
            tokio::spawn(durable::run(
                durable::Context {
                    pool,
                    plugins,
                    archive,
                    pricing,
                    memory: memory.clone(),
                    metrics: metrics.clone(),
                    journal: journal.clone(),
                },
                receiver,
                worker_count,
            ))
        } else {
            tokio::spawn(run_dispatcher(
                pool,
                plugins,
                archive,
                pricing,
                receiver,
                worker_count,
                metrics.clone(),
            ))
        };
        Ok((
            Self {
                sender,
                queue_capacity,
                memory,
                dropped: Arc::new(AtomicU64::new(0)),
                metrics,
                journal,
            },
            TracePipeline { handle },
        ))
    }

    pub fn record(&self, event: TraceEvent) {
        let Some(reservation) = self.memory.reserve(event_bytes(&event)) else {
            self.metrics.trace_dropped_memory();
            self.warn_dropped("trace memory budget is full");
            return;
        };
        match self.sender.try_send(QueuedTrace { event, reservation }) {
            Ok(()) => {
                self.metrics.trace_enqueued();
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.metrics.trace_dropped_full();
                self.warn_dropped("trace queue is full");
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.metrics.trace_dropped_closed();
                self.warn_dropped("trace pipeline is stopped");
            }
        }
    }

    pub fn queue_metrics(&self) -> TraceQueueMetrics {
        let mut metrics = TraceQueueMetrics::new(self.queue_capacity, self.sender.capacity());
        metrics.memory_limit_bytes = self.memory.limit as u64;
        metrics.memory_used_bytes = self.memory.used() as u64;
        metrics.journal = self
            .journal
            .as_ref()
            .map(|j| j.snapshot(self.memory.limit))
            .unwrap_or_default();
        metrics
    }

    fn warn_dropped(&self, reason: &str) {
        let count = self.dropped.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        // Avoid an unbounded warning stream on the live path during overload.
        // Exact counters remain available through /metrics and /api/stats.
        if count.is_power_of_two() {
            tracing::warn!(reason, dropped = count, "dropping trace events");
        }
    }
}

async fn run_dispatcher(
    pool: PgPool,
    plugins: Arc<PluginManager>,
    archive: crate::config::ArchiveConfig,
    pricing: Arc<crate::pricing::PriceTable>,
    mut receiver: mpsc::Receiver<QueuedTrace>,
    worker_count: usize,
    metrics: RuntimeMetrics,
) {
    let mut workers = JoinSet::new();

    while let Some(queued) = receiver.recv().await {
        while workers.len() >= worker_count {
            workers.join_next().await;
        }
        let pool = pool.clone();
        let plugins = plugins.clone();
        let archive = archive.clone();
        let pricing = pricing.clone();
        let metrics = metrics.clone();
        workers.spawn(async move {
            process_trace(pool, plugins, archive, pricing, queued.event, metrics).await;
            // Include queued AND actively processed traces in the budget.
            drop(queued.reservation);
        });
    }

    while workers.join_next().await.is_some() {}
}

async fn process_trace(
    pool: PgPool,
    plugins: Arc<PluginManager>,
    archive: crate::config::ArchiveConfig,
    pricing: Arc<crate::pricing::PriceTable>,
    event: TraceEvent,
    metrics: RuntimeMetrics,
) {
    let trace_id = event.id;
    let built = build_priced_trace(event, plugins, &pricing, &metrics).await;
    let (trace, messages, user_id, user_name) = match built {
        Ok(built) => built,
        Err(error) => {
            tracing::error!(%trace_id, %error, "failed to build trace");
            return;
        }
    };
    if let Err(error) =
        storage::insert_trace(&pool, &archive, trace, messages, user_id, user_name).await
    {
        metrics.trace_persist_failed();
        tracing::error!(%trace_id, %error, "failed to persist trace");
    } else {
        metrics.trace_persisted();
    }
}

async fn build_priced_trace(
    event: TraceEvent,
    plugins: Arc<PluginManager>,
    pricing: &crate::pricing::PriceTable,
    metrics: &RuntimeMetrics,
) -> anyhow::Result<BuiltTrace> {
    let result = tokio::task::spawn_blocking(move || build_trace(event, &plugins))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|result| result);
    let (mut trace, messages, user_id, user_name) =
        result.inspect_err(|_| metrics.trace_build_failed())?;
    if trace.usage_complete {
        trace.estimated_cost_microusd = trace
            .model
            .as_ref()
            .and_then(|model| pricing.get(model))
            .and_then(|price| price.estimate_microusd(&trace.usage));
    }
    Ok((trace, messages, user_id, user_name))
}

pub(crate) fn build_trace(
    event: TraceEvent,
    plugins: &PluginManager,
) -> anyhow::Result<BuiltTrace> {
    let parsed = parsers::parse_trace(
        &event.original_uri,
        &event.request_body,
        &event.response_body,
    );

    let mut metadata = object_or_empty(event.plugin_metadata);
    let mut tags = event.tags;
    let mut session_key = event.session_key;
    let mut user_id = event.user_id;
    let mut user_name = event.user_name;

    if event.run_plugins && plugins.has_plugins() {
        let start_effects = plugins.run_hook(
            PluginHook::RequestStart,
            HookInput {
                hook: PluginHook::RequestStart,
                trace_id: event.id.to_string(),
                method: event.method.clone(),
                uri: event.original_uri.clone(),
                upstream_url: event.upstream_url.clone(),
                headers: event.plugin_request_headers.clone(),
                body_utf8: String::from_utf8(event.request_body.clone()).ok(),
            },
        );
        merge_effects(
            &mut metadata,
            &mut tags,
            &mut session_key,
            &mut user_id,
            &mut user_name,
            start_effects,
        );
        if event.status.is_some() {
            let response_effects = plugins.run_hook(
                PluginHook::ResponseHeaders,
                HookInput {
                    hook: PluginHook::ResponseHeaders,
                    trace_id: event.id.to_string(),
                    method: event.method.clone(),
                    uri: event.original_uri.clone(),
                    upstream_url: event.upstream_url.clone(),
                    headers: event.plugin_response_headers.clone(),
                    body_utf8: None,
                },
            );
            let end_effects = plugins.run_hook(
                PluginHook::ResponseEnd,
                HookInput {
                    hook: PluginHook::ResponseEnd,
                    trace_id: event.id.to_string(),
                    method: event.method.clone(),
                    uri: event.original_uri.clone(),
                    upstream_url: event.upstream_url.clone(),
                    headers: event.plugin_response_headers.clone(),
                    body_utf8: String::from_utf8(event.response_body.clone()).ok(),
                },
            );
            merge_effects(
                &mut metadata,
                &mut tags,
                &mut session_key,
                &mut user_id,
                &mut user_name,
                response_effects,
            );
            merge_effects(
                &mut metadata,
                &mut tags,
                &mut session_key,
                &mut user_id,
                &mut user_name,
                end_effects,
            );
        }
    }

    let explicit_session_key = session_key.or(parsed.session_key_hint);
    if explicit_session_key.is_none() {
        tags.push("session_unlinked".to_string());
    }
    // A credential identifies an account, never a conversation. Scope client
    // hints by upstream and credential to avoid merging unrelated employees.
    let session_key = Some(scoped_session_key(
        &event.upstream_url,
        event.api_key_hash.as_deref(),
        explicit_session_key.as_deref(),
        event.id,
    ));
    if parsed.response.tool_calls_truncated {
        tags.push("tool_calls_truncated".to_string());
    }
    let ttft_ms = event.ttft_ms.or_else(|| {
        parsed.response.first_output_offset.and_then(|offset| {
            let index = event
                .response_chunk_timings
                .partition_point(|(end, _)| *end < offset);
            event
                .response_chunk_timings
                .get(index)
                .map(|(_, elapsed)| *elapsed)
        })
    });
    let (session_key, user_id, user_name, identity_truncated) =
        bound_trace_identity_fields(session_key, user_id, user_name);

    let mut messages = event.messages;
    messages.extend(parsed.messages);
    let (messages, messages_truncated) = bound_session_messages(messages);
    let tags = bound_trace_tags(tags, messages_truncated, identity_truncated);
    let original_uri = redaction::redact_uri_query_values(&event.original_uri);
    let upstream_url = redaction::redact_uri_query_values(&event.upstream_url);
    let incomplete_stream = parsed.response.stream_complete == Some(false)
        && !event.response_body_truncated
        && event
            .status
            .is_some_and(|status| (200..300).contains(&status))
        && event
            .content_type
            .as_deref()
            .is_some_and(|value| value.starts_with("text/event-stream"))
        && matches!(
            parsed.request_kind,
            RequestKind::OpenAiChatCompletions
                | RequestKind::OpenAiResponses
                | RequestKind::AnthropicMessages
        );

    let trace = TraceRecord {
        id: event.id,
        started_at: event.started_at,
        completed_at: event.completed_at,
        method: event.method,
        original_uri,
        upstream_url,
        upstream_host: event.upstream_host,
        status: event.status,
        error: event.error.or(parsed.response.error).or_else(|| {
            incomplete_stream.then(|| "upstream stream ended without a terminal event".to_string())
        }),
        request_kind: event.request_kind.unwrap_or(parsed.request_kind),
        model: event.model.or(parsed.model),
        api_key_hash: event.api_key_hash,
        session_key,
        session_id: None,
        ttft_ms,
        ttfb_ms: event.ttfb_ms,
        usage: parsed.response.usage,
        usage_complete: parsed.response.usage_complete && !event.response_body_truncated,
        tool_calls: parsed.response.tool_calls,
        estimated_cost_microusd: None,
        duration_ms: event.duration_ms,
        bytes_in: event.request_body_bytes,
        bytes_out: event.response_body_bytes,
        request_headers: event.request_headers,
        response_headers: event.response_headers,
        request_body: event.request_body,
        response_body: event.response_body,
        request_body_bytes: event.request_body_bytes,
        response_body_bytes: event.response_body_bytes,
        request_body_truncated: event.request_body_truncated,
        response_body_truncated: event.response_body_truncated,
        content_type: event.content_type,
        plugin_metadata: Value::Object(metadata),
        tags,
    };

    Ok((trace, messages, user_id, user_name))
}

fn scoped_session_key(
    upstream: &str,
    credential: Option<&str>,
    hint: Option<&str>,
    trace_id: Uuid,
) -> String {
    use sha2::{Digest, Sha256};
    let Some(hint) = hint.filter(|hint| !hint.is_empty()) else {
        return format!("request:{trace_id}");
    };
    let origin = url::Url::parse(upstream)
        .map(|url| url.origin().ascii_serialization())
        .unwrap_or_default();
    let scope = serde_json::to_vec(&(origin, credential, hint)).unwrap_or_default();
    format!("session:{}", hex::encode(Sha256::digest(scope)))
}

fn bound_trace_identity_fields(
    session_key: Option<String>,
    user_id: Option<String>,
    user_name: Option<String>,
) -> (Option<String>, Option<String>, Option<String>, bool) {
    let (session_key, session_key_truncated) =
        bound_optional_text(session_key, MAX_TRACE_SESSION_KEY_BYTES);
    let (user_id, user_id_truncated) = bound_optional_text(user_id, MAX_TRACE_IDENTITY_BYTES);
    let (user_name, user_name_truncated) = bound_optional_text(user_name, MAX_TRACE_IDENTITY_BYTES);

    (
        session_key,
        user_id,
        user_name,
        session_key_truncated || user_id_truncated || user_name_truncated,
    )
}

fn bound_optional_text(value: Option<String>, max_bytes: usize) -> (Option<String>, bool) {
    let Some(value) = value else {
        return (None, false);
    };
    if value.is_empty() {
        return (None, false);
    }
    let (value, truncated) = truncate_utf8_owned(value, max_bytes);
    (Some(value), truncated)
}

fn bound_trace_tags(
    tags: Vec<String>,
    messages_truncated: bool,
    identity_truncated: bool,
) -> Vec<String> {
    let mut normalized = Vec::new();
    let mut tags_truncated = false;

    for tag in tags {
        if tag.is_empty() {
            continue;
        }
        let (tag, tag_truncated) = truncate_utf8_owned(tag, MAX_TRACE_TAG_BYTES);
        tags_truncated |= tag_truncated;
        if !normalized.contains(&tag) {
            normalized.push(tag);
        }
    }

    let mut required = Vec::new();
    if messages_truncated {
        required.push(SESSION_MESSAGES_TRUNCATED_TAG);
    }
    let user_capacity_without_enrichment = MAX_TRACE_TAGS.saturating_sub(required.len());
    if identity_truncated || tags_truncated || normalized.len() > user_capacity_without_enrichment {
        required.push(TRACE_ENRICHMENT_TRUNCATED_TAG);
    }

    let user_capacity = MAX_TRACE_TAGS.saturating_sub(required.len());
    let mut bounded = Vec::with_capacity(MAX_TRACE_TAGS.min(normalized.len() + required.len()));
    for tag in normalized {
        if required.contains(&tag.as_str()) {
            continue;
        }
        if bounded.len() >= user_capacity {
            continue;
        }
        bounded.push(tag);
    }
    for tag in required {
        if !bounded.iter().any(|existing| existing == tag) {
            bounded.push(tag.to_string());
        }
    }

    bounded
}

fn bound_session_messages(mut messages: Vec<ParsedMessage>) -> (Vec<ParsedMessage>, bool) {
    let mut truncated = messages.len() > MAX_SESSION_MESSAGES_PER_TRACE;
    // Retain both initial context and the newest turn, including tool output.
    if truncated {
        let keep_start = MAX_SESSION_MESSAGES_PER_TRACE / 2;
        let keep_end = messages.len() - (MAX_SESSION_MESSAGES_PER_TRACE - keep_start);
        messages.drain(keep_start..keep_end);
    }
    let mut bounded = Vec::with_capacity(messages.len().min(MAX_SESSION_MESSAGES_PER_TRACE));

    for message in messages.into_iter().take(MAX_SESSION_MESSAGES_PER_TRACE) {
        let (role, role_truncated) =
            truncate_utf8_owned(message.role, MAX_SESSION_MESSAGE_ROLE_BYTES);
        let (content, content_truncated) =
            truncate_utf8_owned(message.content, MAX_SESSION_MESSAGE_CONTENT_BYTES);
        truncated |= role_truncated || content_truncated;
        bounded.push(ParsedMessage { role, content });
    }

    (bounded, truncated)
}

fn truncate_utf8_owned(value: String, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value, false);
    }

    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_string(), true)
}

fn merge_effects(
    metadata: &mut Map<String, Value>,
    tags: &mut Vec<String>,
    session_key: &mut Option<String>,
    user_id: &mut Option<String>,
    user_name: &mut Option<String>,
    effects: PluginEffects,
) {
    for (key, value) in effects.metadata {
        match (metadata.get_mut(&key), value) {
            (Some(Value::Object(existing)), Value::Object(fields)) => existing.extend(fields),
            (_, value) => {
                metadata.insert(key, value);
            }
        }
    }
    tags.extend(effects.tags);
    if !effects.warnings.is_empty() {
        metadata
            .entry("plugin_warnings".to_string())
            .or_insert_with(|| json!([]));
        if let Some(existing) = metadata
            .get_mut("plugin_warnings")
            .and_then(Value::as_array_mut)
        {
            existing.extend(effects.warnings.into_iter().map(Value::String));
        }
    }
    if session_key.is_none() {
        *session_key = effects.session_key;
    }
    if user_id.is_none() {
        *user_id = effects.user_id;
    }
    if user_name.is_none() {
        *user_name = effects.user_name;
    }
}

fn object_or_empty(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other if other.is_null() => Map::new(),
        other => {
            let mut map = Map::new();
            map.insert("value".to_string(), other);
            map
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_need_explicit_hints_and_are_scoped_to_upstream_and_credential() {
        let id = Uuid::new_v4();
        let key = scoped_session_key(
            "https://llm.example/v1",
            Some("alice"),
            Some("conversation"),
            id,
        );
        assert_eq!(
            key,
            scoped_session_key(
                "https://llm.example/v1/responses",
                Some("alice"),
                Some("conversation"),
                Uuid::new_v4()
            )
        );
        assert_ne!(
            key,
            scoped_session_key(
                "https://llm.example/v1",
                Some("bob"),
                Some("conversation"),
                id
            )
        );
        assert_ne!(
            key,
            scoped_session_key(
                "https://other.example/v1",
                Some("alice"),
                Some("conversation"),
                id
            )
        );
        assert_ne!(
            scoped_session_key("", Some("alice"), None, id),
            scoped_session_key("", Some("alice"), None, Uuid::new_v4())
        );
    }

    #[test]
    fn first_output_timing_uses_the_chunk_containing_the_complete_event() {
        let plugins = PluginManager::load(&[]).unwrap();
        let mut event = TraceEvent::base(Uuid::new_v4(), Utc::now());
        event.original_uri = "/v1/chat/completions".into();
        event.response_body = b"data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n".to_vec();
        event.ttfb_ms = Some(10);
        event.response_chunk_timings = vec![
            (50, 10),
            (event.response_body.len() - 5, 25),
            (event.response_body.len(), 80),
        ];
        let (trace, _, _, _) = build_trace(event, &plugins).unwrap();
        assert_eq!(trace.ttfb_ms, Some(10));
        assert_eq!(trace.ttft_ms, Some(80));
    }

    #[test]
    fn long_transcript_preview_keeps_the_latest_response() {
        let mut messages = (0..200)
            .map(|i| ParsedMessage {
                role: "user".into(),
                content: i.to_string(),
            })
            .collect::<Vec<_>>();
        messages.push(ParsedMessage {
            role: "assistant".into(),
            content: "latest reply".into(),
        });
        let (preview, truncated) = bound_session_messages(messages);
        assert!(truncated);
        assert_eq!(preview.first().unwrap().content, "0");
        assert_eq!(preview.last().unwrap().content, "latest reply");
    }

    #[test]
    fn missing_stream_terminator_is_an_error_only_when_capture_is_complete() {
        let plugins = PluginManager::load(&[]).unwrap();
        for truncated in [false, true] {
            let mut event = TraceEvent::base(Uuid::new_v4(), Utc::now());
            event.original_uri = "/v1/responses".into();
            event.content_type = Some("text/event-stream".into());
            event.status = Some(200);
            event.response_body =
                b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n".to_vec();
            event.response_body_truncated = truncated;
            let (trace, _, _, _) = build_trace(event, &plugins).unwrap();
            assert_eq!(trace.error.is_some(), !truncated);
        }
    }

    #[test]
    fn trace_recorder_queue_metrics_reports_pending_events() {
        let (sender, _receiver) = mpsc::channel(2);
        let recorder = TraceRecorder {
            sender,
            queue_capacity: 2,
            memory: MemoryBudget::new(4096),
            dropped: Arc::new(AtomicU64::new(0)),
            metrics: RuntimeMetrics::default(),
            journal: None,
        };

        assert_eq!(recorder.queue_metrics().depth, 0);
        assert_eq!(recorder.queue_metrics().memory_used_bytes, 0);

        recorder.record(TraceEvent::base(Uuid::new_v4(), Utc::now()));

        assert_eq!(recorder.queue_metrics().depth, 1);
        assert!(recorder.queue_metrics().memory_used_bytes > 0);
        assert_eq!(recorder.queue_metrics().memory_limit_bytes, 4096);
    }

    #[test]
    fn bound_trace_identity_fields_drops_empty_and_truncates_values() {
        let session_key = Some("s".repeat(MAX_TRACE_SESSION_KEY_BYTES + 1));
        let user_id = Some(String::new());
        let user_name = Some(format!("{}é", "u".repeat(MAX_TRACE_IDENTITY_BYTES - 1)));

        let (session_key, user_id, user_name, truncated) =
            bound_trace_identity_fields(session_key, user_id, user_name);

        assert!(truncated);
        assert_eq!(session_key.unwrap().len(), MAX_TRACE_SESSION_KEY_BYTES);
        assert!(user_id.is_none());
        let user_name = user_name.unwrap();
        assert_eq!(user_name.len(), MAX_TRACE_IDENTITY_BYTES - 1);
        assert!(user_name.is_char_boundary(user_name.len()));
    }

    #[test]
    fn queued_and_in_progress_events_share_the_memory_budget() {
        let event = || {
            let mut event = TraceEvent::base(Uuid::new_v4(), Utc::now());
            event.request_body = vec![0; 1024];
            event
        };
        let bytes = event_bytes(&event());
        let (sender, mut receiver) = mpsc::channel(8);
        let recorder = TraceRecorder {
            sender,
            queue_capacity: 8,
            memory: MemoryBudget::new(bytes * 2),
            dropped: Arc::new(AtomicU64::new(0)),
            metrics: RuntimeMetrics::default(),
            journal: None,
        };
        recorder.record(event());
        let processing = receiver.try_recv().unwrap();
        assert_eq!(recorder.queue_metrics().depth, 0);
        assert_eq!(recorder.memory.used(), bytes);
        recorder.record(event());
        recorder.record(event());
        assert_eq!(recorder.memory.used(), bytes * 2);
        assert_eq!(
            recorder.metrics.snapshot(recorder.queue_metrics())["trace_pipeline"]["dropped_memory"],
            1
        );
        drop(processing);
        recorder.record(event());
        assert_eq!(recorder.queue_metrics().depth, 2);
        drop(receiver);
        assert_eq!(recorder.memory.used(), 0);
    }

    #[test]
    fn rejected_queue_sends_release_their_memory_reservations() {
        let (sender, receiver) = mpsc::channel(1);
        let recorder = TraceRecorder {
            sender,
            queue_capacity: 1,
            memory: MemoryBudget::new(1024 * 1024),
            dropped: Arc::new(AtomicU64::new(0)),
            metrics: RuntimeMetrics::default(),
            journal: None,
        };
        let event = || TraceEvent::base(Uuid::new_v4(), Utc::now());
        recorder.record(event());
        let bytes = recorder.memory.used();
        recorder.record(event());
        assert_eq!(recorder.memory.used(), bytes);
        assert_eq!(
            recorder.metrics.snapshot(recorder.queue_metrics())["trace_pipeline"]["dropped_full"],
            1
        );
        drop(receiver);
        recorder.record(event());
        assert_eq!(recorder.memory.used(), 0);
        assert_eq!(
            recorder.metrics.snapshot(recorder.queue_metrics())["trace_pipeline"]["dropped_closed"],
            1
        );
    }

    #[test]
    fn bound_trace_tags_caps_deduplicates_and_preserves_required_markers() {
        let mut tags = (0..MAX_TRACE_TAGS)
            .map(|index| format!("tag-{index}"))
            .collect::<Vec<_>>();
        tags.push("tag-1".to_string());

        let bounded = bound_trace_tags(tags, true, false);

        assert_eq!(bounded.len(), MAX_TRACE_TAGS);
        assert_eq!(bounded.iter().filter(|tag| *tag == "tag-1").count(), 1);
        assert!(
            bounded
                .iter()
                .any(|tag| tag == SESSION_MESSAGES_TRUNCATED_TAG)
        );
        assert!(
            bounded
                .iter()
                .any(|tag| tag == TRACE_ENRICHMENT_TRUNCATED_TAG)
        );
    }

    #[test]
    fn bound_trace_tags_truncates_long_tag_on_utf8_boundary() {
        let tag = format!("{}é", "t".repeat(MAX_TRACE_TAG_BYTES - 1));

        let bounded = bound_trace_tags(vec![tag], false, false);

        assert_eq!(bounded[0].len(), MAX_TRACE_TAG_BYTES - 1);
        assert!(bounded[0].is_char_boundary(bounded[0].len()));
        assert!(
            bounded
                .iter()
                .any(|tag| tag == TRACE_ENRICHMENT_TRUNCATED_TAG)
        );
    }

    #[test]
    fn bound_session_messages_allows_small_messages() {
        let messages = vec![ParsedMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
        }];

        let (bounded, truncated) = bound_session_messages(messages);

        assert!(!truncated);
        assert_eq!(bounded.len(), 1);
        assert_eq!(bounded[0].role, "user");
        assert_eq!(bounded[0].content, "hello");
    }

    #[test]
    fn build_trace_redacts_uri_query_values_before_persistence() {
        let plugins = PluginManager::load(&[]).unwrap();
        let mut event = TraceEvent::base(Uuid::new_v4(), Utc::now());
        event.method = "GET".to_string();
        event.original_uri = "/v1/messages?api_key=sk-secret&debug=true".to_string();
        event.upstream_url =
            "https://api.example.com/v1/messages?api_key=sk-secret&debug=true".to_string();
        event.upstream_host = Some("api.example.com".to_string());

        let (trace, _, _, _) = build_trace(event, &plugins).unwrap();

        assert_eq!(
            trace.original_uri,
            "/v1/messages?api_key=REDACTED&debug=REDACTED"
        );
        assert_eq!(
            trace.upstream_url,
            "https://api.example.com/v1/messages?api_key=REDACTED&debug=REDACTED"
        );
    }

    #[test]
    fn bound_session_messages_caps_count_and_field_lengths() {
        let role = "r".repeat(MAX_SESSION_MESSAGE_ROLE_BYTES + 1);
        let content = "c".repeat(MAX_SESSION_MESSAGE_CONTENT_BYTES + 1);
        let messages = (0..=MAX_SESSION_MESSAGES_PER_TRACE)
            .map(|_| ParsedMessage {
                role: role.clone(),
                content: content.clone(),
            })
            .collect();

        let (bounded, truncated) = bound_session_messages(messages);

        assert!(truncated);
        assert_eq!(bounded.len(), MAX_SESSION_MESSAGES_PER_TRACE);
        assert_eq!(bounded[0].role.len(), MAX_SESSION_MESSAGE_ROLE_BYTES);
        assert_eq!(bounded[0].content.len(), MAX_SESSION_MESSAGE_CONTENT_BYTES);
    }

    #[test]
    fn bound_session_messages_truncates_on_utf8_boundaries() {
        let role = format!("{}é", "r".repeat(MAX_SESSION_MESSAGE_ROLE_BYTES - 1));
        let content = format!("{}é", "c".repeat(MAX_SESSION_MESSAGE_CONTENT_BYTES - 1));
        let messages = vec![ParsedMessage { role, content }];

        let (bounded, truncated) = bound_session_messages(messages);

        assert!(truncated);
        assert_eq!(bounded[0].role.len(), MAX_SESSION_MESSAGE_ROLE_BYTES - 1);
        assert!(bounded[0].role.is_char_boundary(bounded[0].role.len()));
        assert_eq!(
            bounded[0].content.len(),
            MAX_SESSION_MESSAGE_CONTENT_BYTES - 1
        );
        assert!(
            bounded[0]
                .content
                .is_char_boundary(bounded[0].content.len())
        );
    }
}
