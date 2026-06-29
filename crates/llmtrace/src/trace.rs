use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use sqlx::PgPool;
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};
use uuid::Uuid;

use crate::metrics::RuntimeMetrics;
use crate::parsers;
use crate::plugins::{HookInput, PluginEffects, PluginManager};
use crate::redaction;
use crate::storage::{self, ParsedMessage, TraceRecord};
use crate::types::{BodyRedaction, PluginHook, RequestKind};

#[derive(Debug)]
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
    pub duration_ms: Option<i64>,
    pub request_body_bytes: i64,
    pub response_body_bytes: i64,
    pub request_headers: Value,
    pub response_headers: Value,
    pub plugin_request_headers: Value,
    pub plugin_response_headers: Value,
    pub request_body: Vec<u8>,
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
    sender: mpsc::Sender<TraceEvent>,
    queue_capacity: usize,
    metrics: RuntimeMetrics,
}

/// Owns the background dispatcher task so it can be drained on shutdown.
pub struct TracePipeline {
    handle: JoinHandle<()>,
}

impl TracePipeline {
    /// Waits for in-flight traces to finish once all recorders have been dropped.
    pub async fn drain(self) {
        if tokio::time::timeout(Duration::from_secs(10), self.handle)
            .await
            .is_err()
        {
            tracing::warn!("trace pipeline drain timed out");
        }
    }
}

pub fn fallback_session_key(host: Option<&str>, api_key_hash: Option<&str>) -> Option<String> {
    let host = host.unwrap_or("unknown-host");
    api_key_hash
        .map(|hash| format!("{host}:{hash}"))
        .or_else(|| Some(format!("{host}:anonymous")))
}

impl TraceRecorder {
    pub fn spawn(
        pool: PgPool,
        plugins: Arc<PluginManager>,
        body_redaction: BodyRedaction,
        queue_capacity: usize,
        worker_count: usize,
        metrics: RuntimeMetrics,
    ) -> (Self, TracePipeline) {
        let queue_capacity = queue_capacity.max(1);
        let worker_count = worker_count.max(1);
        let (sender, receiver) = mpsc::channel(queue_capacity);

        tracing::info!(queue_capacity, worker_count, "trace pipeline started");
        let handle = tokio::spawn(run_dispatcher(
            pool,
            plugins,
            body_redaction,
            receiver,
            worker_count,
            metrics.clone(),
        ));

        (
            Self {
                sender,
                queue_capacity,
                metrics,
            },
            TracePipeline { handle },
        )
    }

    pub fn record(&self, event: TraceEvent) {
        let trace_id = event.id;
        match self.sender.try_send(event) {
            Ok(()) => {
                self.metrics.trace_enqueued();
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.metrics.trace_dropped_full();
                tracing::warn!(
                    %trace_id,
                    queue_capacity = self.queue_capacity,
                    "trace queue is full; dropping trace"
                );
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.metrics.trace_dropped_closed();
                tracing::warn!(%trace_id, "trace pipeline is stopped; dropping trace");
            }
        }
    }
}

async fn run_dispatcher(
    pool: PgPool,
    plugins: Arc<PluginManager>,
    body_redaction: BodyRedaction,
    mut receiver: mpsc::Receiver<TraceEvent>,
    worker_count: usize,
    metrics: RuntimeMetrics,
) {
    let mut workers = JoinSet::new();

    while let Some(event) = receiver.recv().await {
        while workers.len() >= worker_count {
            workers.join_next().await;
        }
        workers.spawn(process_trace(
            pool.clone(),
            plugins.clone(),
            body_redaction,
            event,
            metrics.clone(),
        ));
    }

    while workers.join_next().await.is_some() {}
}

async fn process_trace(
    pool: PgPool,
    plugins: Arc<PluginManager>,
    body_redaction: BodyRedaction,
    event: TraceEvent,
    metrics: RuntimeMetrics,
) {
    let trace_id = event.id;
    let built =
        tokio::task::spawn_blocking(move || build_trace(event, &plugins, body_redaction)).await;
    let result = match built {
        Ok(result) => result,
        Err(error) => {
            metrics.trace_build_failed();
            tracing::error!(%trace_id, error = %error, "trace build task failed");
            return;
        }
    };
    let (trace, messages, user_id, user_name) = match result {
        Ok(value) => value,
        Err(error) => {
            metrics.trace_build_failed();
            tracing::error!(%trace_id, error = %error, "failed to build trace");
            return;
        }
    };

    if let Err(error) = storage::insert_trace(&pool, trace, messages, user_id, user_name).await {
        metrics.trace_persist_failed();
        tracing::error!(%trace_id, error = %error, "failed to persist trace");
    } else {
        metrics.trace_persisted();
    }
}

fn build_trace(
    event: TraceEvent,
    plugins: &PluginManager,
    body_redaction: BodyRedaction,
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

    if event.run_plugins {
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

    let session_key = session_key.or(parsed.session_key_hint).or_else(|| {
        fallback_session_key(
            event.upstream_host.as_deref(),
            event.api_key_hash.as_deref(),
        )
    });

    let mut messages = event.messages;
    messages.extend(parsed.messages);

    let trace = TraceRecord {
        id: event.id,
        started_at: event.started_at,
        completed_at: event.completed_at,
        method: event.method,
        original_uri: event.original_uri,
        upstream_url: event.upstream_url,
        upstream_host: event.upstream_host,
        status: event.status,
        error: event.error,
        request_kind: event.request_kind.unwrap_or(parsed.request_kind),
        model: event.model.or(parsed.model),
        api_key_hash: event.api_key_hash,
        session_key,
        session_id: None,
        ttft_ms: event.ttft_ms,
        duration_ms: event.duration_ms,
        bytes_in: event.request_body_bytes,
        bytes_out: event.response_body_bytes,
        request_headers: event.request_headers,
        response_headers: event.response_headers,
        request_body_compressed: storage::compress(&redaction::redact_body(
            &event.request_body,
            body_redaction,
        ))?,
        response_body_compressed: storage::compress(&redaction::redact_body(
            &event.response_body,
            body_redaction,
        ))?,
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

fn merge_effects(
    metadata: &mut Map<String, Value>,
    tags: &mut Vec<String>,
    session_key: &mut Option<String>,
    user_id: &mut Option<String>,
    user_name: &mut Option<String>,
    effects: PluginEffects,
) {
    for (key, value) in effects.metadata {
        metadata.insert(key, value);
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
