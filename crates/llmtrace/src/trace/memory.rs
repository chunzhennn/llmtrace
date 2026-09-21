use super::TraceEvent;
use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug)]
pub(super) struct MemoryBudget {
    pub limit: usize,
    used: AtomicUsize,
}

impl MemoryBudget {
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit: limit.max(1),
            used: AtomicUsize::new(0),
        })
    }

    pub fn used(&self) -> usize {
        self.used.load(Ordering::Relaxed)
    }

    pub fn reserve(self: &Arc<Self>, bytes: usize) -> Option<Reservation> {
        self.used
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                used.checked_add(bytes).filter(|next| *next <= self.limit)
            })
            .ok()?;
        Some(Reservation {
            budget: self.clone(),
            bytes,
        })
    }
}

#[derive(Debug)]
pub(super) struct Reservation {
    budget: Arc<MemoryBudget>,
    bytes: usize,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

// Account for allocated body capacity (Vec growth can exceed its length),
// timestamp buffers and metadata. Worker parsing/compression scratch space and
// live proxy captures are separate from this queued/in-progress event budget.
pub(super) fn event_bytes(event: &TraceEvent) -> usize {
    let mut size = std::mem::size_of::<TraceEvent>()
        .saturating_add(event.request_body.capacity())
        .saturating_add(event.response_body.capacity())
        .saturating_add(
            event
                .response_chunk_timings
                .capacity()
                .saturating_mul(std::mem::size_of::<(usize, i64)>()),
        );
    for value in [
        &event.request_headers,
        &event.response_headers,
        &event.plugin_request_headers,
        &event.plugin_response_headers,
        &event.plugin_metadata,
    ] {
        size = size.saturating_add(json_bytes(value));
    }
    for text in [&event.method, &event.original_uri, &event.upstream_url]
        .into_iter()
        .chain(
            [
                &event.upstream_host,
                &event.error,
                &event.model,
                &event.api_key_hash,
                &event.credential_scope_hash,
                &event.session_key,
                &event.content_type,
                &event.user_id,
                &event.user_name,
            ]
            .into_iter()
            .filter_map(Option::as_ref),
        )
    {
        size = size.saturating_add(text.capacity());
    }
    size = size.saturating_add(
        event
            .tags
            .capacity()
            .saturating_mul(std::mem::size_of::<String>()),
    );
    for tag in &event.tags {
        size = size.saturating_add(tag.capacity());
    }
    size = size.saturating_add(
        event
            .messages
            .capacity()
            .saturating_mul(std::mem::size_of::<crate::types::ParsedMessage>()),
    );
    for message in &event.messages {
        size = size
            .saturating_add(message.role.capacity())
            .saturating_add(message.content.capacity());
    }
    size
}

fn json_bytes(value: &Value) -> usize {
    std::mem::size_of::<Value>().saturating_add(match value {
        Value::String(text) => text.capacity(),
        Value::Array(items) => items.iter().fold(
            items
                .capacity()
                .saturating_mul(std::mem::size_of::<Value>()),
            |n, v| n.saturating_add(json_bytes(v)),
        ),
        Value::Object(fields) => fields.iter().fold(0usize, |n, (k, v)| {
            n.saturating_add(k.capacity())
                .saturating_add(json_bytes(v))
                .saturating_add(64)
        }),
        _ => 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservations_bound_concurrent_usage_and_release_on_drop() {
        let budget = MemoryBudget::new(100);
        let held = budget.reserve(60).unwrap();
        assert!(budget.reserve(41).is_none());
        let remaining = budget.reserve(40).unwrap();
        assert_eq!(budget.used(), 100);
        drop(held);
        assert_eq!(budget.used(), 40);
        drop(remaining);
        assert_eq!(budget.used(), 0);
        assert!(budget.reserve(usize::MAX).is_none());
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let budget = &budget;
                scope.spawn(move || {
                    for _ in 0..1000 {
                        if let Some(_held) = budget.reserve(30) {
                            assert!(budget.used() <= 100);
                        }
                    }
                });
            }
        });
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn event_budget_counts_capacity_and_metadata_without_serializing_bodies() {
        let mut event = TraceEvent::base(uuid::Uuid::new_v4(), chrono::Utc::now());
        let empty = event_bytes(&event);
        event.request_body = Vec::with_capacity(1024);
        event.response_body = Vec::with_capacity(2048);
        assert_eq!(event_bytes(&event), empty + 3072);
        event.plugin_metadata = serde_json::json!({"frames":[{"text":"x".repeat(4096)}]});
        assert!(event_bytes(&event) >= empty + 3072 + 4096);
    }
}
