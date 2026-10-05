# User identity lookup from a WASM plugin

Plugins execute after traffic completes, in the background trace workers. The host now provides an optional HTTP GET import; WASI and unrestricted sockets are not enabled. The actual user lookup URL and JSON schema depend on your enterprise gateway. `/v1/whoami` below is an example, not a standard LLM API endpoint.

```toml
[[plugins]]
name = "user-identity"
wasm_path = "plugins/user-identity.wasm"
hooks = ["on_request_start"]
timeout_ms = 1000
http_get_urls = ["https://llm.example.com/v1/whoami"]
```

The allowlist compares parsed, normalized URLs exactly, including scheme, port, path, and query. It does not permit other paths on the same host. Redirects are returned to the plugin and never followed. HTTP is supported for internal services; use HTTPS when the connection needs TLS. Plugin lookup permissions are separate from proxy upstream permissions.

The WASM import is:

```c
// import module: "llmtrace", name: "http_get"
int32_t http_get(int32_t request_ptr, int32_t request_len,
                 int32_t output_ptr, int32_t output_capacity);
```

Both buffers belong to the plugin's exported linear memory. The request is UTF-8 JSON:

```json
{
  "url": "https://llm.example.com/v1/whoami",
  "headers": { "authorization": "Bearer user-api-key" }
}
```

Read the credential from `HookInput.headers` in the request-start hook. Only send the credential header required by the configured identity endpoint. The host never supplies a credential automatically. `Host`, `Connection`, `Content-Length`, and `Transfer-Encoding` overrides are rejected.

The result is a positive byte count written into the output buffer, containing:

```json
{
  "status": 200,
  "body": "{\"id\":\"user-123\",\"name\":\"Ada\"}"
}
```

`body` is a string. Parse it according to the upstream schema and check the HTTP status before using its identity. Return the existing hook output, for example:

```json
{
  "user_id": "user-123",
  "user_name": "Ada",
  "custom_fields": { "identity_source": "gateway" }
}
```

Only return `session_key` when you have an actual conversation identifier. A username or API key groups a user's unrelated conversations together and must not be used as a substitute. Existing hook exports, allocation, and packed output-pointer conventions remain unchanged. Legacy `metadata` is accepted as an alias of `custom_fields`.

The HTTP import returns `-1` for a denied URL, invalid buffer/request, timeout, transport failure, non-UTF-8 body, oversized response, or insufficient output capacity. Return a warning and leave identity unset on failure; do not fabricate a username. HTTP error responses themselves return the JSON envelope so the plugin can handle their status.

Limits per invocation:

- Four HTTP calls at most, sharing the plugin's wall-clock deadline.
- 16 KiB request JSON; 64 KiB response body and at most 64 KiB output capacity. JSON escaping adds envelope overhead, so a near-limit body can still exceed the output buffer.
- 128 MiB linear memory, one memory and instance, 100,000 table elements.
- Existing 64 KiB hook output limit and epoch timeout remain in force.

The host uses the Tokio runtime from the blocking trace worker; it does not block a live proxy handler. Each invocation receives a fresh instance. Prefer a request-start-only lookup hook, give network lookups a realistic timeout, and monitor trace queue/persistence metrics. These permissions allow a trusted plugin to transmit captured credentials to the configured endpoint, so review the plugin and its exact URLs together.

## Shared plugin KV cache

Plugins get a fresh WASM instance per invocation and cannot keep state, so the host also offers a process-wide bounded KV store through two additional imports (module `llmtrace`):

```c
// Both buffers below belong to the plugin's exported linear memory.
// Returns the bytes written, 0 for a miss, and -1 for a rejected call.
int32_t cache_get(int32_t key_ptr, int32_t key_len, int32_t output_ptr, int32_t output_capacity);
// Returns 0 when accepted, and -1 for a rejected call.
int32_t cache_put(int32_t key_ptr, int32_t key_len, int32_t value_ptr, int32_t value_len, int32_t ttl_secs);
```

The division of labor is deliberate: the plugin owns the caching semantics — what to key on, what to store, how fresh it must be (`ttl_secs = 0` uses the configured default) — while the host enforces the bounds a plugin cannot uphold itself: at most 256-byte keys and 16 KiB values, a per-entry TTL capped at 3600 seconds, a bounded entry count, and race-free least-recently-used eviction across the concurrent trace workers (a plugin-side order ledger would need read-modify-write cycles the workers race on). Keys are opaque bytes hashed before they rest in memory, so a plugin that passes a raw credential as a key still never leaves the secret in cache structures — but prefer hashing in the plugin anyway so the digest doubles as a stable identifier.

Treat both calls as optional optimizations: with `plugin_cache.capacity = 0` every `cache_get` misses and every `cache_put` is rejected, so caching must never be load-bearing for correctness.

```toml
[plugin_cache]
capacity = 512  # entries; 0 disables the store and its metrics
ttl_secs = 300  # default TTL for cache_put calls passing 0
```

Environment overrides: `LLMTRACE_PLUGIN_CACHE_CAPACITY` and `LLMTRACE_PLUGIN_CACHE_TTL_SECS`. Capacity is capped at 100000 entries and the default TTL at 3600 seconds. Size capacity for the distinct keys active within one TTL window; values are capped at 16 KiB, so the default bounds memory to a few MiB. Hit, miss, and store counters are exported as `llmtrace_plugin_cache_hits_total`, `llmtrace_plugin_cache_misses_total`, and `llmtrace_plugin_cache_stores_total` on `/metrics`, and in the `/api/metrics` JSON under `plugins.cache`.

## Bundled LiteLLM identity plugin

`crates/litellm-user-plugin` builds the reference plugin for a LiteLLM upstream. It reads the caller's credential from the unredacted request headers (`authorization`, `x-api-key` or `api-key`) and resolves it through the gateway's self-service endpoints, which accept the key itself and need no master key:

- `GET {origin}/key/info` — returns `user_id`, `team_id`, `org_id`, `key_alias` for the calling key.
- `GET {origin}/user/info` — returns `user_email`, `user_alias`, `user_role`, `teams` for the calling key's user.

The origin is derived from the trace's `upstream_url`, so one binary serves any deployment; pin both URLs in `http_get_urls`. Output populates `user_id`/`user_name` plus `custom_fields` (`key_alias`, `team_id`, `org_id`, `user_email`, `user_alias`, `user_role`, `teams`, `litellm_key_hash`, `identity_source`). `litellm_key_hash` is the SHA-256 of the bare key — the same digest LiteLLM stores in `LiteLLM_VerificationToken.token` — so traces can be joined against LiteLLM tables offline without persisting the raw credential. Team or service keys without a user get the `litellm_key_without_user` tag; failed lookups get `litellm_identity_lookup_failed` plus a warning; the plugin never fabricates an identity and never sets `session_key`.

The plugin is also the reference consumer of the KV cache: it keys on `litellm_key_hash` and stores a compact identity document (< 1 KiB), so a hit skips both gateway calls entirely. Fully resolved identities — including team keys without a user — are cached for one hour: the primary attribution (`user_id`, `user_email`) is effectively immutable per key, while an expiring entry still picks up drifting secondary fields (a key's reassigned `team_id`, SSO-synced `teams`/`user_role`). Failed or partial resolutions are never cached and retry on the next trace; a hit emits output identical to a fresh resolution.

```sh
cargo build -p litellm-user-plugin --target wasm32-unknown-unknown --release
```

```toml
[[plugins]]
name = "litellm-user"
wasm_path = "target/wasm32-unknown-unknown/release/litellm_user_plugin.wasm"
hooks = ["on_request_start"]
timeout_ms = 5000
http_get_urls = ["http://litellm:4000/key/info", "http://litellm:4000/user/info"]
```

The test `plugins::tests::litellm_user_plugin_resolves_identity_and_caches_in_the_plugin_kv` loads the built module through the real host, resolves identity against a mock gateway, and verifies the second invocation is served from the plugin KV cache with no additional gateway requests.

The test `plugins::http::tests::http_host_looks_up_identity_without_following_redirects_and_times_out` exercises a real local HTTP server through the WASM import. The actual enterprise identity schema still needs to be supplied and implemented by the deployment's plugin.
