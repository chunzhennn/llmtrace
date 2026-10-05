//! Bounded shared KV cache exposed to plugins through WASM imports.
//!
//! Plugins get a fresh WASM instance per invocation and cannot keep state, so
//! the host offers this process-wide store instead. Everything semantic —
//! what to key on, what to store, how fresh it must be — belongs to the
//! plugin; the host only guarantees the invariants a plugin cannot uphold
//! itself: bounded entry count, bounded key/value sizes, per-entry TTL, and
//! race-free LRU eviction across the concurrent trace workers (a plugin-side
//! order ledger would need read-modify-write cycles that the workers race on).
//!
//! Keys are opaque bytes and are hashed before they rest in memory, so a
//! plugin that passes a raw credential as a key still never leaves the secret
//! in cache structures. `cache_get` returns `0` for a miss and `-1` for a
//! rejected call; `cache_put` returns `0` on success and `-1` on rejection.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use wasmtime::{Caller, Linker};

use super::http::HostState;
use crate::metrics::RuntimeMetrics;

const MAX_KEY_BYTES: usize = 256;
const MAX_VALUE_BYTES: usize = 16 * 1024;
/// Upper bound for per-entry TTL; `cache_put` with 0 uses the configured
/// default, matching `[plugin_cache] ttl_secs`.
pub(super) const MAX_ENTRY_TTL_SECS: u64 = 3600;

pub(super) struct PluginCache {
    inner: Mutex<CacheState>,
    capacity: usize,
    default_ttl: Duration,
    metrics: RuntimeMetrics,
}

#[derive(Default)]
struct CacheState {
    entries: HashMap<String, CachedValue>,
    /// Front is the least recently used key; mirrors `entries` exactly.
    order: VecDeque<String>,
}

struct CachedValue {
    value: Vec<u8>,
    expires_at: Instant,
}

impl PluginCache {
    pub fn new(capacity: usize, default_ttl_secs: u64, metrics: RuntimeMetrics) -> Self {
        Self {
            inner: Mutex::new(CacheState::default()),
            capacity,
            default_ttl: Duration::from_secs(default_ttl_secs),
            metrics,
        }
    }

    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        if self.capacity == 0 {
            return None;
        }
        let digest = crate::redaction::sha256_hex(key);
        let mut state = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        match state
            .entries
            .get(&digest)
            .map(|entry| (entry.value.clone(), entry.expires_at))
        {
            Some((value, expires_at)) if Instant::now() < expires_at => {
                if let Some(position) = state
                    .order
                    .iter()
                    .position(|candidate| *candidate == digest)
                {
                    state.order.remove(position);
                    state.order.push_back(digest);
                }
                self.metrics.plugin_cache_hit();
                Some(value)
            }
            Some(_) => {
                state.entries.remove(&digest);
                state.order.retain(|candidate| *candidate != digest);
                self.metrics.plugin_cache_miss();
                None
            }
            None => {
                self.metrics.plugin_cache_miss();
                None
            }
        }
    }

    fn put(&self, key: &[u8], value: &[u8], ttl_secs: u64) -> bool {
        if self.capacity == 0
            || key.len() > MAX_KEY_BYTES
            || value.len() > MAX_VALUE_BYTES
            || ttl_secs > MAX_ENTRY_TTL_SECS
        {
            return false;
        }
        let ttl = if ttl_secs == 0 {
            self.default_ttl
        } else {
            Duration::from_secs(ttl_secs)
        };
        let digest = crate::redaction::sha256_hex(key);
        let mut state = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if !state.entries.contains_key(&digest) {
            // Evict before inserting so a full cache never grows past capacity.
            while state.entries.len() >= self.capacity {
                match state.order.pop_front() {
                    Some(evicted) => {
                        state.entries.remove(&evicted);
                    }
                    None => break,
                }
            }
        }
        if state
            .entries
            .insert(
                digest.clone(),
                CachedValue {
                    value: value.to_vec(),
                    expires_at: Instant::now() + ttl,
                },
            )
            .is_some()
            && let Some(position) = state
                .order
                .iter()
                .position(|candidate| *candidate == digest)
        {
            state.order.remove(position);
        }
        state.order.push_back(digest);
        self.metrics.plugin_cache_store();
        true
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entries
            .len()
    }
}

pub(super) fn register(linker: &mut Linker<HostState>) -> anyhow::Result<()> {
    // All buffers belong to the plugin's linear memory. cache_get returns the
    // bytes written, 0 for a miss, and -1 for a rejected call; cache_put
    // returns 0 on success and -1 on rejection.
    linker.func_wrap(
        "llmtrace",
        "cache_get",
        |mut caller: Caller<'_, HostState>,
         key_ptr: i32,
         key_len: i32,
         out_ptr: i32,
         out_capacity: i32|
         -> i32 {
            cache_get(&mut caller, key_ptr, key_len, out_ptr, out_capacity).unwrap_or(-1)
        },
    )?;
    linker.func_wrap(
        "llmtrace",
        "cache_put",
        |mut caller: Caller<'_, HostState>,
         key_ptr: i32,
         key_len: i32,
         value_ptr: i32,
         value_len: i32,
         ttl_secs: i32|
         -> i32 {
            cache_put(
                &mut caller,
                key_ptr,
                key_len,
                value_ptr,
                value_len,
                ttl_secs,
            )
            .unwrap_or(-1)
        },
    )?;
    Ok(())
}

fn cache_get(
    caller: &mut Caller<'_, HostState>,
    key_ptr: i32,
    key_len: i32,
    out_ptr: i32,
    out_capacity: i32,
) -> anyhow::Result<i32> {
    if key_ptr < 0 || key_len <= 0 || key_len as usize > MAX_KEY_BYTES {
        anyhow::bail!("invalid plugin cache key bounds");
    }
    let memory = caller
        .get_export("memory")
        .and_then(|export| export.into_memory())
        .ok_or_else(|| anyhow::anyhow!("missing memory"))?;
    let mut key = vec![0; key_len as usize];
    memory.read(&*caller, key_ptr as usize, &mut key)?;
    let Some(value) = caller.data().cache.get(&key) else {
        return Ok(0);
    };
    if out_ptr < 0 || out_capacity < 0 {
        anyhow::bail!("invalid plugin cache output bounds");
    }
    if value.len() > out_capacity as usize {
        anyhow::bail!("plugin cache value exceeds output capacity");
    }
    memory.write(caller, out_ptr as usize, &value)?;
    Ok(value.len() as i32)
}

fn cache_put(
    caller: &mut Caller<'_, HostState>,
    key_ptr: i32,
    key_len: i32,
    value_ptr: i32,
    value_len: i32,
    ttl_secs: i32,
) -> anyhow::Result<i32> {
    if key_ptr < 0
        || key_len <= 0
        || key_len as usize > MAX_KEY_BYTES
        || value_ptr < 0
        || value_len <= 0
        || value_len as usize > MAX_VALUE_BYTES
        || ttl_secs < 0
        || ttl_secs as u64 > MAX_ENTRY_TTL_SECS
    {
        anyhow::bail!("invalid plugin cache put bounds");
    }
    let memory = caller
        .get_export("memory")
        .and_then(|export| export.into_memory())
        .ok_or_else(|| anyhow::anyhow!("missing memory"))?;
    let mut key = vec![0; key_len as usize];
    memory.read(&*caller, key_ptr as usize, &mut key)?;
    let mut value = vec![0; value_len as usize];
    memory.read(&*caller, value_ptr as usize, &mut value)?;
    if caller.data().cache.put(&key, &value, ttl_secs as u64) {
        Ok(0)
    } else {
        anyhow::bail!("plugin cache rejected the entry");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cache(capacity: usize, default_ttl_secs: u64) -> PluginCache {
        PluginCache::new(capacity, default_ttl_secs, RuntimeMetrics::default())
    }

    #[test]
    fn plugin_cache_stores_and_reads_values_by_opaque_keys() {
        let cache = test_cache(4, 300);
        assert!(cache.put(b"key", b"value", 300));
        assert_eq!(cache.get(b"key"), Some(b"value".to_vec()));
        assert_eq!(cache.get(b"other"), None);
    }

    #[test]
    fn plugin_cache_evicts_least_recently_used_entries() {
        let cache = test_cache(2, 300);
        cache.put(b"a", b"1", 300);
        cache.put(b"b", b"2", 300);
        assert_eq!(cache.get(b"a"), Some(b"1".to_vec()));
        cache.put(b"c", b"3", 300);

        assert_eq!(cache.get(b"b"), None, "b was the least recently used key");
        assert_eq!(cache.get(b"a"), Some(b"1".to_vec()));
        assert_eq!(cache.get(b"c"), Some(b"3".to_vec()));
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn plugin_cache_expires_entries_after_their_ttl() {
        let cache = test_cache(4, 0);
        assert!(cache.put(b"a", b"1", 0), "ttl 0 falls back to the default");
        assert_eq!(
            cache.get(b"a"),
            None,
            "a zero default ttl makes every entry immediately stale"
        );
    }

    #[test]
    fn plugin_cache_rejects_oversized_and_out_of_range_input() {
        let cache = test_cache(4, 300);
        assert!(!cache.put(&vec![0; MAX_KEY_BYTES + 1], b"v", 300));
        assert!(!cache.put(b"k", &vec![0; MAX_VALUE_BYTES + 1], 300));
        assert!(!cache.put(b"k", b"v", MAX_ENTRY_TTL_SECS + 1));

        let disabled = test_cache(0, 300);
        assert!(!disabled.put(b"k", b"v", 300));
        assert_eq!(disabled.get(b"k"), None);
    }

    #[test]
    fn plugin_cache_replaces_values_without_growing() {
        let cache = test_cache(2, 300);
        cache.put(b"a", b"old", 300);
        cache.put(b"a", b"new", 300);
        assert_eq!(cache.get(b"a"), Some(b"new".to_vec()));
        assert_eq!(cache.len(), 1);
    }
}
