//! llmtrace WASM plugin that attributes traces to LiteLLM users.
//!
//! On `llmtrace_on_request_start` the plugin reads the caller's LiteLLM
//! credential from the unredacted hook headers and resolves it through the
//! gateway's self-service endpoints, which need no master key:
//!
//! - `GET {origin}/key/info` — defaults to the key in the Authorization
//!   header; a key may always look up itself. Yields `user_id`, `team_id`,
//!   `org_id` and `key_alias`.
//! - `GET {origin}/user/info` — defaults to the key's own user. Yields
//!   `user_email`, `user_alias`, `user_role` and `teams`.
//!
//! The origin is derived from the trace's `upstream_url`, so one binary serves
//! any deployment; the exact lookup URLs stay pinned in llmtrace's
//! `http_get_urls` allowlist.
//!
//! ## Plugin-side caching
//!
//! WASM instances are fresh per invocation, so the plugin caches through the
//! host's shared bounded KV store (`cache_get`/`cache_put`). The plugin owns
//! the caching semantics: the key is the SHA-256 of the bare key — the same
//! digest LiteLLM stores in `LiteLLM_VerificationToken.token`, enabling
//! offline joins — and the value is a compact identity document, not a gateway
//! response. The host only bounds memory (entry count, value size) and evicts
//! least-recently-used entries race-free. Policy:
//!
//! - a fully resolved identity (including "team key without a user") is
//!   cached for [`IDENTITY_CACHE_TTL_SECS`];
//! - failed, partial, or unparseable resolutions are never cached, so the
//!   next trace retries them;
//! - a hit produces byte-identical hook output to a fresh resolution.
//!
//! Failures degrade to warnings and tags; identity is never fabricated, and
//! `session_key` is never set because an API key is not a conversation id.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

const KEY_INFO_PATH: &str = "/key/info";
const USER_INFO_PATH: &str = "/user/info";
const MAX_FIELD_BYTES: usize = 256;
const MAX_TEAMS: usize = 16;
/// How long a fully resolved identity is reused before the gateway is asked
/// again, and also the host's per-entry TTL cap. The primary attribution
/// (`user_id` -> SSO identity, `user_email`) is effectively immutable per key,
/// which is why this is hours, not seconds. It is still finite because the
/// secondary fields do drift: LiteLLM can reassign a key's `team_id`, and the
/// deployment's SSO sync updates `teams`/`user_role` on every login — an
/// unexpired entry would attribute traces to stale teams until eviction.
/// One refresh per key per hour is negligible gateway load.
const IDENTITY_CACHE_TTL_SECS: i32 = 3600;
/// Bumped when the cached document layout changes; older entries refetch.
const CACHE_DOC_VERSION: i64 = 1;
/// Mirrors the host's `MAX_VALUE_BYTES`; larger capacities are rejected.
#[cfg(target_arch = "wasm32")]
const HOST_CACHE_CAPACITY: i32 = 16 * 1024;
/// Mirrors the host's `MAX_RESPONSE_BYTES`; larger capacities are rejected.
#[cfg(target_arch = "wasm32")]
const HOST_LOOKUP_CAPACITY: i32 = 64 * 1024;

/// One credential header the gateway accepts, in lookup preference order.
const CREDENTIAL_HEADERS: &[&str] = &["authorization", "x-api-key", "api-key"];

#[derive(Clone)]
pub struct LookupResponse {
    pub status: u16,
    pub body: String,
}

/// The host's shared bounded KV store, as seen by the plugin.
pub trait CacheBackend {
    /// Returns the stored value for `key`, or `None` on a miss or rejection.
    fn get(&self, key: &[u8]) -> Option<Vec<u8>>;
    /// Stores `value` under `key` for `ttl_secs`; returns whether it was
    /// accepted.
    fn put(&self, key: &[u8], value: &[u8], ttl_secs: u32) -> bool;
}

/// Performs one allowlisted gateway lookup, mirroring the host `http_get`.
type Lookup<'a> = &'a mut dyn FnMut(&str, &BTreeMap<String, String>) -> Option<LookupResponse>;

/// Resolves the hook input into a `HookOutput` JSON value. `lookup` performs
/// the host `http_get` call and `cache` the host KV read/write (or canned
/// stand-ins under test).
pub fn identity(input: &Value, lookup: Lookup<'_>, cache: &dyn CacheBackend) -> Value {
    let mut warnings: Vec<String> = Vec::new();
    let mut tags: Vec<String> = Vec::new();
    let mut custom_fields: Map<String, Value> = Map::new();
    let mut user_id: Option<String> = None;
    let mut user_name: Option<String> = None;

    let Some((credential_header, credential_value)) = input
        .get("headers")
        .and_then(Value::as_object)
        .and_then(find_credential)
    else {
        tags.push("litellm_no_credential".to_string());
        return hook_output(user_id, user_name, custom_fields, tags, warnings);
    };

    let bare_key = bare_key(credential_header, credential_value);
    let key_hash = sha256_hex(bare_key.as_bytes());
    custom_fields.insert("litellm_key_hash".to_string(), json!(key_hash));
    custom_fields.insert("identity_source".to_string(), json!("litellm"));

    if let Some(document) = cached_identity(cache, key_hash.as_bytes()) {
        return emit_cached(document, custom_fields, tags, warnings);
    }

    let Some(origin) = origin_of(
        input
            .get("upstream_url")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    ) else {
        warnings.push("litellm plugin could not derive an origin from upstream_url".to_string());
        return hook_output(user_id, user_name, custom_fields, tags, warnings);
    };

    let mut send_headers = BTreeMap::new();
    send_headers.insert(credential_header.to_string(), credential_value.to_string());

    let key_document = lookup_json(
        &format!("{origin}{KEY_INFO_PATH}"),
        &send_headers,
        lookup,
        "litellm key lookup",
        &mut warnings,
    );
    let mut key_lookup_failed = key_document.is_none();
    if let Some(info) = key_document
        .as_ref()
        .and_then(|document| document.get("info"))
        .and_then(Value::as_object)
    {
        key_lookup_failed = false;
        copy_string_field(info, "key_alias", &mut custom_fields);
        copy_string_field(info, "team_id", &mut custom_fields);
        copy_string_field(info, "org_id", &mut custom_fields);
        user_id = info
            .get("user_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(bounded);
    }
    if key_lookup_failed {
        // Failed lookups are not cached: the next trace retries them.
        tags.push("litellm_identity_lookup_failed".to_string());
        return hook_output(user_id, user_name, custom_fields, tags, warnings);
    }

    let mut user_lookup_failed = false;
    if user_id.is_some() {
        if let Some(info) = lookup_json(
            &format!("{origin}{USER_INFO_PATH}"),
            &send_headers,
            lookup,
            "litellm user lookup",
            &mut warnings,
        )
        .as_ref()
        .and_then(|document| document.get("user_info"))
        .and_then(Value::as_object)
        {
            let email = info.get("user_email").and_then(Value::as_str).map(bounded);
            let alias = info.get("user_alias").and_then(Value::as_str).map(bounded);
            user_name = email.as_ref().or(alias.as_ref()).cloned();
            if let Some(email) = email {
                custom_fields.insert("user_email".to_string(), json!(email));
            }
            if let Some(alias) = alias {
                custom_fields.insert("user_alias".to_string(), json!(alias));
            }
            copy_string_field(info, "user_role", &mut custom_fields);
            if let Some(teams) = info.get("teams").and_then(Value::as_array) {
                let teams: Vec<String> = teams
                    .iter()
                    .filter_map(Value::as_str)
                    .map(bounded)
                    .take(MAX_TEAMS)
                    .collect();
                if !teams.is_empty() {
                    custom_fields.insert("teams".to_string(), json!(teams));
                }
            }
        } else {
            user_lookup_failed = true;
        }
    } else {
        // Team or service keys carry no end user; keep the key metadata.
        tags.push("litellm_key_without_user".to_string());
    }

    // Cache only complete resolutions: full identity, or a key that genuinely
    // has no user. Partial failures retry on the next trace.
    if !user_lookup_failed {
        let document = json!({
            "v": CACHE_DOC_VERSION,
            "user_id": user_id,
            "user_name": user_name,
            "custom_fields": custom_fields,
            "tags": tags,
        });
        cache.put(
            key_hash.as_bytes(),
            document.to_string().as_bytes(),
            IDENTITY_CACHE_TTL_SECS as u32,
        );
    }

    hook_output(user_id, user_name, custom_fields, tags, warnings)
}

/// Returns the cached identity document for `key` if one is stored and
/// readable; unreadable entries (version skew, corruption) are treated as a
/// miss and overwritten by the next resolution.
fn cached_identity(cache: &dyn CacheBackend, key: &[u8]) -> Option<Value> {
    let bytes = cache.get(key)?;
    let document = serde_json::from_slice::<Value>(&bytes).ok()?;
    let compatible = document.get("v").and_then(Value::as_i64) == Some(CACHE_DOC_VERSION)
        && document
            .get("custom_fields")
            .and_then(Value::as_object)
            .is_some();
    compatible.then_some(document)
}

fn emit_cached(
    document: Value,
    mut custom_fields: Map<String, Value>,
    mut tags: Vec<String>,
    warnings: Vec<String>,
) -> Value {
    let user_id = document
        .get("user_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let user_name = document
        .get("user_name")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(fields) = document.get("custom_fields").and_then(Value::as_object) {
        for (name, value) in fields {
            custom_fields.insert(name.clone(), value.clone());
        }
    }
    if let Some(cached_tags) = document.get("tags").and_then(Value::as_array) {
        tags.extend(
            cached_tags
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string),
        );
    }
    hook_output(user_id, user_name, custom_fields, tags, warnings)
}

fn hook_output(
    user_id: Option<String>,
    user_name: Option<String>,
    custom_fields: Map<String, Value>,
    tags: Vec<String>,
    warnings: Vec<String>,
) -> Value {
    json!({
        "user_id": user_id,
        "user_name": user_name,
        "custom_fields": custom_fields,
        "tags": tags,
        "warnings": warnings,
    })
}

fn lookup_json(
    url: &str,
    headers: &BTreeMap<String, String>,
    lookup: Lookup<'_>,
    what: &str,
    warnings: &mut Vec<String>,
) -> Option<Value> {
    match lookup(url, headers) {
        None => {
            warnings.push(format!("{what} failed"));
            None
        }
        Some(response) if response.status != 200 => {
            warnings.push(format!("{what} returned status {}", response.status));
            None
        }
        Some(response) => match serde_json::from_str(&response.body) {
            Ok(document) => Some(document),
            Err(_) => {
                warnings.push(format!("{what} returned an unparseable body"));
                None
            }
        },
    }
}

fn find_credential(headers: &Map<String, Value>) -> Option<(&str, &str)> {
    CREDENTIAL_HEADERS.iter().find_map(|name| {
        headers
            .get(*name)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty() && *value != "<non-utf8>")
            .map(|value| (*name, value))
    })
}

/// Strips an optional auth scheme (`Bearer`, `Basic`, ...) so the hashed value
/// matches LiteLLM's `hash_token(key)` of the bare key.
fn bare_key<'a>(header: &str, value: &'a str) -> &'a str {
    if header == "authorization"
        && let Some((scheme, rest)) = value.split_once(' ')
        && !scheme.is_empty()
        && scheme.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return rest.trim();
    }
    value.trim()
}

/// `scheme://authority` of the forwarded URL; the gateway route is replaced.
fn origin_of(upstream_url: &str) -> Option<String> {
    let (scheme, rest) = upstream_url.split_once("://")?;
    if !matches!(scheme, "http" | "https") {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{authority}"))
}

fn copy_string_field(source: &Map<String, Value>, field: &str, target: &mut Map<String, Value>) {
    if let Some(value) = source.get(field).and_then(Value::as_str)
        && !value.is_empty()
    {
        target.insert(field.to_string(), json!(bounded(value)));
    }
}

/// Caps a field at `MAX_FIELD_BYTES` on a UTF-8 character boundary.
fn bounded(value: &str) -> String {
    let mut out = String::with_capacity(value.len().min(MAX_FIELD_BYTES));
    for character in value.chars() {
        if out.len() + character.len_utf8() > MAX_FIELD_BYTES {
            break;
        }
        out.push(character);
    }
    out
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

// ---------------------------------------------------------------------------
// llmtrace WASM ABI: exported allocator and hook, plus the http_get and
// cache_get/cache_put imports.
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn llmtrace_alloc(len: i32) -> i32 {
    let Ok(len) = usize::try_from(len) else {
        return 0;
    };
    if len == 0 || len > 16 * 1024 * 1024 {
        return 0;
    }
    let mut buffer: Vec<u8> = Vec::with_capacity(len);
    let pointer = buffer.as_mut_ptr();
    core::mem::forget(buffer);
    pointer as i32
}

/// `len` must equal the capacity requested from `llmtrace_alloc`, which the
/// host guarantees for both the hook input and the hook output buffers.
#[unsafe(no_mangle)]
pub extern "C" fn llmtrace_dealloc(pointer: i32, len: i32) {
    let (Ok(pointer), Ok(len)) = (usize::try_from(pointer), usize::try_from(len)) else {
        return;
    };
    if pointer == 0 || len == 0 {
        return;
    }
    // Capacity-only reconstruction: the buffer content belongs to the host.
    unsafe { drop(Vec::from_raw_parts(pointer as *mut u8, 0, len)) };
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "llmtrace")]
unsafe extern "C" {
    #[link_name = "http_get"]
    fn http_get(request_ptr: i32, request_len: i32, output_ptr: i32, output_capacity: i32) -> i32;
    #[link_name = "cache_get"]
    fn cache_get(key_ptr: i32, key_len: i32, output_ptr: i32, output_capacity: i32) -> i32;
    #[link_name = "cache_put"]
    fn cache_put(key_ptr: i32, key_len: i32, value_ptr: i32, value_len: i32, ttl_secs: i32) -> i32;
}

/// Performs one allowlisted `http_get` through the host import. The output
/// buffer is reclaimed by reconstructing the allocation as a `Vec`, so it must
/// not be passed to `llmtrace_dealloc` afterwards.
#[cfg(target_arch = "wasm32")]
fn host_lookup(url: &str, headers: &BTreeMap<String, String>) -> Option<LookupResponse> {
    let Ok(request) = serde_json::to_vec(&json!({"url": url, "headers": headers})) else {
        return None;
    };
    let request_len = i32::try_from(request.len()).ok()?;
    let request_ptr = llmtrace_alloc(request_len);
    if request_ptr == 0 {
        return None;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(request.as_ptr(), request_ptr as *mut u8, request.len());
    }
    let output_ptr = llmtrace_alloc(HOST_LOOKUP_CAPACITY);
    if output_ptr == 0 {
        llmtrace_dealloc(request_ptr, request_len);
        return None;
    }
    let written = unsafe { http_get(request_ptr, request_len, output_ptr, HOST_LOOKUP_CAPACITY) };
    llmtrace_dealloc(request_ptr, request_len);
    if written <= 0 {
        llmtrace_dealloc(output_ptr, HOST_LOOKUP_CAPACITY);
        return None;
    }
    let envelope = unsafe {
        Vec::from_raw_parts(
            output_ptr as *mut u8,
            written as usize,
            HOST_LOOKUP_CAPACITY as usize,
        )
    };
    let envelope: Value = serde_json::from_slice(&envelope).ok()?;
    Some(LookupResponse {
        status: envelope.get("status").and_then(Value::as_u64)? as u16,
        body: envelope.get("body").and_then(Value::as_str)?.to_string(),
    })
}

/// Bridges the host KV imports. Output buffers are reclaimed by reconstructing
/// the allocation as a `Vec`, so they must not be passed to
/// `llmtrace_dealloc` afterwards.
#[cfg(target_arch = "wasm32")]
struct HostCache;

#[cfg(target_arch = "wasm32")]
impl CacheBackend for HostCache {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        let key_len = i32::try_from(key.len()).ok()?;
        let key_ptr = llmtrace_alloc(key_len);
        if key_ptr == 0 {
            return None;
        }
        unsafe { std::ptr::copy_nonoverlapping(key.as_ptr(), key_ptr as *mut u8, key.len()) };
        let output_ptr = llmtrace_alloc(HOST_CACHE_CAPACITY);
        if output_ptr == 0 {
            llmtrace_dealloc(key_ptr, key_len);
            return None;
        }
        let written = unsafe { cache_get(key_ptr, key_len, output_ptr, HOST_CACHE_CAPACITY) };
        llmtrace_dealloc(key_ptr, key_len);
        if written <= 0 {
            // -1 is a rejected call, 0 is a miss; both degrade to None.
            llmtrace_dealloc(output_ptr, HOST_CACHE_CAPACITY);
            return None;
        }
        Some(unsafe {
            Vec::from_raw_parts(
                output_ptr as *mut u8,
                written as usize,
                HOST_CACHE_CAPACITY as usize,
            )
        })
    }

    fn put(&self, key: &[u8], value: &[u8], ttl_secs: u32) -> bool {
        let (Ok(key_len), Ok(value_len)) = (i32::try_from(key.len()), i32::try_from(value.len()))
        else {
            return false;
        };
        let key_ptr = llmtrace_alloc(key_len);
        if key_ptr == 0 {
            return false;
        }
        unsafe { std::ptr::copy_nonoverlapping(key.as_ptr(), key_ptr as *mut u8, key.len()) };
        let value_ptr = llmtrace_alloc(value_len);
        if value_ptr == 0 {
            llmtrace_dealloc(key_ptr, key_len);
            return false;
        }
        unsafe { std::ptr::copy_nonoverlapping(value.as_ptr(), value_ptr as *mut u8, value.len()) };
        let accepted =
            unsafe { cache_put(key_ptr, key_len, value_ptr, value_len, ttl_secs as i32) };
        llmtrace_dealloc(key_ptr, key_len);
        llmtrace_dealloc(value_ptr, value_len);
        accepted == 0
    }
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn llmtrace_on_request_start(pointer: i32, len: i32) -> i64 {
    let input = if pointer > 0 && len > 0 {
        let bytes = unsafe { std::slice::from_raw_parts(pointer as *const u8, len as usize) };
        serde_json::from_slice::<Value>(bytes).ok()
    } else {
        None
    };
    let output = match input {
        Some(input) => identity(&input, &mut host_lookup, &HostCache),
        None => hook_output(
            None,
            None,
            Map::new(),
            Vec::new(),
            vec!["litellm plugin received an unreadable hook input".to_string()],
        ),
    };
    let Ok(bytes) = serde_json::to_vec(&output) else {
        return 0;
    };
    let Ok(output_len) = i32::try_from(bytes.len()) else {
        return 0;
    };
    let output_ptr = llmtrace_alloc(output_len);
    if output_ptr == 0 {
        return 0;
    }
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), output_ptr as *mut u8, bytes.len()) };
    ((output_ptr as i64) << 32) | (output_len as u32 as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// HashMap-backed cache stand-in that also records TTLs and access.
    #[derive(Default)]
    struct MapCache {
        entries: RefCell<HashMap<CacheKey, CacheEntry>>,
    }

    type CacheKey = Vec<u8>;
    type CacheEntry = (Vec<u8>, u32);

    impl MapCache {
        fn is_empty(&self) -> bool {
            self.entries.borrow().is_empty()
        }
    }

    impl CacheBackend for MapCache {
        fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
            self.entries.borrow().get(key).map(|(v, _)| v.clone())
        }

        fn put(&self, key: &[u8], value: &[u8], ttl_secs: u32) -> bool {
            self.entries
                .borrow_mut()
                .insert(key.to_vec(), (value.to_vec(), ttl_secs));
            true
        }
    }

    fn hook_input(headers: Value) -> Value {
        json!({
            "hook": "on_request_start",
            "trace_id": "0b7b8f8e-1111-4222-8333-444455556666",
            "method": "POST",
            "uri": "/v1/chat/completions",
            "upstream_url": "http://litellm:4000/v1/chat/completions",
            "headers": headers,
            "body_utf8": null,
        })
    }

    fn key_info(user_id: Option<&str>) -> LookupResponse {
        LookupResponse {
            status: 200,
            body: json!({
                "key": "sk-1234",
                "info": {
                    // LiteLLM pops the hashed token before responding, so a
                    // faithful fixture carries no token field at all.
                    "key_alias": "laptop",
                    "team_id": "team-7",
                    "org_id": null,
                    "user_id": user_id,
                }
            })
            .to_string(),
        }
    }

    fn user_info() -> LookupResponse {
        LookupResponse {
            status: 200,
            body: json!({
                "user_id": "authentik-sub-42",
                "user_info": {
                    "user_id": "authentik-sub-42",
                    "user_email": "alice@example.com",
                    "user_alias": null,
                    "user_role": "internal_user",
                    "teams": ["team-7", "lab-b"],
                },
                "keys": [],
                "teams": [],
            })
            .to_string(),
        }
    }

    fn serving_lookup(
        key_response: Option<LookupResponse>,
        user_response: Option<LookupResponse>,
    ) -> impl FnMut(&str, &BTreeMap<String, String>) -> Option<LookupResponse> {
        move |url, _headers| {
            if url.ends_with(KEY_INFO_PATH) {
                key_response.clone()
            } else if url.ends_with(USER_INFO_PATH) {
                user_response.clone()
            } else {
                panic!("unexpected lookup url: {url}");
            }
        }
    }

    #[test]
    fn resolves_user_through_key_and_user_info() {
        let mut lookup =
            serving_lookup(Some(key_info(Some("authentik-sub-42"))), Some(user_info()));

        let output = identity(
            &hook_input(json!({"authorization": "Bearer sk-1234"})),
            &mut lookup,
            &MapCache::default(),
        );

        assert_eq!(output["user_id"], "authentik-sub-42");
        assert_eq!(output["user_name"], "alice@example.com");
        assert_eq!(output["custom_fields"]["key_alias"], "laptop");
        assert_eq!(output["custom_fields"]["team_id"], "team-7");
        assert_eq!(output["custom_fields"]["user_email"], "alice@example.com");
        assert_eq!(output["custom_fields"]["user_role"], "internal_user");
        assert_eq!(output["custom_fields"]["teams"], json!(["team-7", "lab-b"]));
        assert_eq!(output["custom_fields"]["identity_source"], "litellm");
        assert!(output.get("session_key").is_none());
    }

    #[test]
    fn key_hash_covers_bare_key_not_the_bearer_scheme() {
        let mut lookup =
            serving_lookup(Some(key_info(Some("authentik-sub-42"))), Some(user_info()));

        let output = identity(
            &hook_input(json!({"authorization": "Bearer sk-1234"})),
            &mut lookup,
            &MapCache::default(),
        );

        assert_eq!(
            output["custom_fields"]["litellm_key_hash"],
            sha256_hex(b"sk-1234")
        );
    }

    #[test]
    fn second_resolution_is_served_from_the_cache_without_lookups() {
        let mut lookup =
            serving_lookup(Some(key_info(Some("authentik-sub-42"))), Some(user_info()));
        let cache = MapCache::default();
        let input = hook_input(json!({"authorization": "Bearer sk-1234"}));

        let fresh = identity(&input, &mut lookup, &cache);
        let cached = identity(&input, &mut lookup, &cache);

        assert_eq!(fresh, cached, "a hit must emit identical output");
        let (value, ttl) = cache
            .entries
            .borrow()
            .get(sha256_hex(b"sk-1234").as_bytes())
            .cloned()
            .expect("identity document is cached under the bare key hash");
        assert_eq!(ttl, IDENTITY_CACHE_TTL_SECS as u32);
        assert!(
            value.len() < 1024,
            "the cached document stays compact, not a gateway response"
        );
    }

    #[test]
    fn failed_key_lookups_are_not_cached() {
        let mut lookup = serving_lookup(
            Some(LookupResponse {
                status: 401,
                body: "{\"detail\":\"invalid key\"}".to_string(),
            }),
            Some(user_info()),
        );
        let cache = MapCache::default();

        identity(
            &hook_input(json!({"authorization": "Bearer sk-bad"})),
            &mut lookup,
            &cache,
        );

        assert!(cache.is_empty(), "failures must retry on the next trace");
    }

    #[test]
    fn partial_user_failures_are_not_cached() {
        let mut lookup = serving_lookup(Some(key_info(Some("authentik-sub-42"))), None);
        let cache = MapCache::default();

        identity(
            &hook_input(json!({"authorization": "Bearer sk-1234"})),
            &mut lookup,
            &cache,
        );

        assert!(cache.is_empty(), "partial resolutions must retry");
    }

    #[test]
    fn team_keys_without_a_user_are_cached_and_skip_user_lookups() {
        let user_lookups = std::sync::Arc::new(AtomicUsize::new(0));
        let seen = user_lookups.clone();
        let mut lookup = move |url: &str, _headers: &BTreeMap<String, String>| {
            if url.ends_with(KEY_INFO_PATH) {
                Some(key_info(None))
            } else {
                seen.fetch_add(1, Ordering::SeqCst);
                Some(user_info())
            }
        };
        let cache = MapCache::default();
        let input = hook_input(json!({"authorization": "Bearer sk-1234"}));

        let first = identity(&input, &mut lookup, &cache);
        let second = identity(&input, &mut lookup, &cache);

        assert_eq!(first, second);
        assert_eq!(first["user_id"], Value::Null);
        assert!(
            first["tags"]
                .as_array()
                .unwrap()
                .contains(&json!("litellm_key_without_user"))
        );
        assert_eq!(first["custom_fields"]["team_id"], "team-7");
        assert_eq!(
            user_lookups.load(Ordering::SeqCst),
            0,
            "user info must never be fetched for keys without a user"
        );
        assert!(!cache.is_empty());
    }

    #[test]
    fn unreadable_cached_documents_fall_back_to_a_fresh_resolution() {
        let cache = MapCache::default();
        cache.put(
            sha256_hex(b"sk-1234").as_bytes(),
            b"{\"v\":999,\"garbage\":true}".as_slice(),
            300,
        );
        let mut lookup =
            serving_lookup(Some(key_info(Some("authentik-sub-42"))), Some(user_info()));

        let output = identity(
            &hook_input(json!({"authorization": "Bearer sk-1234"})),
            &mut lookup,
            &cache,
        );

        assert_eq!(
            output["user_id"], "authentik-sub-42",
            "version skew refetches"
        );
    }

    #[test]
    fn missing_credential_only_adds_a_tag() {
        let output = identity(
            &hook_input(json!({})),
            &mut |_url, _headers| {
                panic!("no lookup may happen without a credential");
            },
            &MapCache::default(),
        );

        assert_eq!(output["user_id"], Value::Null);
        assert!(
            output["tags"]
                .as_array()
                .unwrap()
                .contains(&json!("litellm_no_credential"))
        );
        assert!(output["warnings"].as_array().unwrap().is_empty());
    }

    #[test]
    fn failed_key_lookup_tags_the_trace_and_never_invents_identity() {
        let mut lookup = serving_lookup(
            Some(LookupResponse {
                status: 401,
                body: "{\"detail\":\"invalid key\"}".to_string(),
            }),
            Some(user_info()),
        );

        let output = identity(
            &hook_input(json!({"authorization": "Bearer sk-bad"})),
            &mut lookup,
            &MapCache::default(),
        );

        assert_eq!(output["user_id"], Value::Null);
        assert!(
            output["tags"]
                .as_array()
                .unwrap()
                .contains(&json!("litellm_identity_lookup_failed"))
        );
        assert!(
            output["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning
                    .as_str()
                    .is_some_and(|text| text.contains("returned status 401")))
        );
        assert_eq!(
            output["custom_fields"]["litellm_key_hash"],
            sha256_hex(b"sk-bad")
        );
    }

    #[test]
    fn failed_user_lookup_keeps_the_key_level_identity() {
        let mut lookup = serving_lookup(Some(key_info(Some("authentik-sub-42"))), None);

        let output = identity(
            &hook_input(json!({"authorization": "Bearer sk-1234"})),
            &mut lookup,
            &MapCache::default(),
        );

        assert_eq!(output["user_id"], "authentik-sub-42");
        assert_eq!(output["user_name"], Value::Null);
        assert!(
            output["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning
                    .as_str()
                    .is_some_and(|text| text.contains("litellm user lookup failed")))
        );
    }

    #[test]
    fn unusable_upstream_url_warns_without_fabricating_identity() {
        let mut input = hook_input(json!({"authorization": "Bearer sk-1234"}));
        input["upstream_url"] = json!("ftp://litellm:4000/v1/chat/completions");

        let output = identity(
            &input,
            &mut |_url, _headers| {
                panic!("no lookup may happen without a usable origin");
            },
            &MapCache::default(),
        );

        assert_eq!(output["user_id"], Value::Null);
        assert!(
            output["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning
                    .as_str()
                    .is_some_and(|text| text.contains("could not derive an origin")))
        );
    }

    #[test]
    fn accepts_x_api_key_credentials() {
        let mut lookup =
            serving_lookup(Some(key_info(Some("authentik-sub-42"))), Some(user_info()));

        let output = identity(
            &hook_input(json!({"x-api-key": "sk-1234"})),
            &mut lookup,
            &MapCache::default(),
        );

        assert_eq!(output["user_id"], "authentik-sub-42");
        assert_eq!(
            output["custom_fields"]["litellm_key_hash"],
            sha256_hex(b"sk-1234")
        );
    }

    #[test]
    fn long_fields_are_bounded_on_character_boundaries() {
        let mut lookup = serving_lookup(
            Some(LookupResponse {
                status: 200,
                body: json!({"info": {
                    "user_id": "u-1",
                    "key_alias": "别名-".repeat(200),
                    "team_id": "t",
                }})
                .to_string(),
            }),
            Some(LookupResponse {
                status: 200,
                body: json!({"user_info": {
                    "user_email": null,
                    "user_alias": "爱".repeat(500),
                }})
                .to_string(),
            }),
        );

        let output = identity(
            &hook_input(json!({"authorization": "Bearer sk-1234"})),
            &mut lookup,
            &MapCache::default(),
        );

        let alias = output["custom_fields"]["key_alias"].as_str().unwrap();
        assert!(alias.len() <= MAX_FIELD_BYTES + 8);
        assert!(alias.starts_with("别名-"));
        assert!(output["user_name"].as_str().unwrap().starts_with('爱'));
    }

    #[test]
    fn origin_extraction_replaces_the_route_but_keeps_authority() {
        assert_eq!(
            origin_of("http://litellm:4000/v1/chat/completions").as_deref(),
            Some("http://litellm:4000")
        );
        assert_eq!(
            origin_of("https://gw.example.com").as_deref(),
            Some("https://gw.example.com")
        );
        assert_eq!(origin_of("litellm:4000"), None);
        assert_eq!(origin_of("https:///v1"), None);
    }

    #[test]
    fn bare_key_strips_known_and_unknown_schemes() {
        assert_eq!(bare_key("authorization", "Bearer sk-1"), "sk-1");
        assert_eq!(bare_key("authorization", "bearer sk-1"), "sk-1");
        assert_eq!(bare_key("authorization", "Basic abc=="), "abc==");
        assert_eq!(bare_key("authorization", "sk-1"), "sk-1");
        assert_eq!(bare_key("x-api-key", " sk-1 "), "sk-1");
    }
}
