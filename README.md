# llmtrace

`llmtrace` is an application-layer reverse proxy for LLM traffic. It forwards HTTP, SSE, and WebSocket traffic to an upstream base URL while recording request/response metadata, redacted headers, bodies, timings, sessions, and plugin-enriched statistics.

This repository currently contains only the Rust backend. The frontend has been removed and should be implemented separately.

## Quick start

1. Start Postgres.
2. Copy `llmtrace.example.toml` to `llmtrace.toml` and update `storage.postgres_url`.
3. Run the server:

   ```bash
   cargo run -p llmtrace -- --config llmtrace.toml
   ```

Proxy traffic is sent to any non-`/api` and non-`/ui` route. HTTP requests are proxied as HTTP, and requests that negotiate a WebSocket upgrade are proxied as WebSocket traffic on the original path. `/ui/*` currently returns a backend placeholder until a new frontend is provided.

## Health and readiness probes

The service exposes unauthenticated probes for production schedulers and load balancers:

- `GET /healthz` returns process liveness and does not touch external dependencies.
- `GET /readyz` verifies PostgreSQL connectivity with a lightweight `SELECT 1`. It returns `503 Service Unavailable` when storage is unavailable.

The Docker image includes a `HEALTHCHECK` against `/readyz`, and `docker-compose.yml` waits for Postgres `pg_isready` before starting `llmtrace`. When running in a container, bind the service to `0.0.0.0:3000`; the compose file sets `LLMTRACE_LISTEN` for that.

## Production mode

Set `server.deployment = "production"` to make startup fail fast on insecure or ambiguous settings. Production mode currently requires:

- `server.public_url` uses `https`.
- `proxy.allow_upstreams` is non-empty, so the service cannot run as an unrestricted open proxy.
- `storage.retention_days` is set, so trace and audit storage growth is bounded.
- `auth.cookie_secure = true`.
- `auth.login_rate_limit.enabled = true`.
- `auth.local_admin.password_hash` is set, and plaintext `auth.local_admin.password` is not set.
- `redaction.body_redaction` is `drop` or `json_secrets`.
- When OAuth is enabled, `auth.oauth.issuer_url` and any explicit `auth.oauth.redirect_url` use `https`, `auth.oauth.require_email_verified = true`, and `auth.oauth.allowed_emails` or `auth.oauth.allowed_domains` is configured. OAuth email allowlist entries must be exact email addresses, and domain entries must be domain names such as `example.com`; URL syntax, wildcards, whitespace, and non-ASCII forms are rejected at startup.

When `server.public_url` uses `https` and `auth.cookie_secure = true`, private UI/API responses include `Strict-Transport-Security: max-age=31536000`. Proxied upstream responses are not modified with this host-level header.

Minimal production-oriented config shape:

```toml
[server]
listen = "0.0.0.0:3000"
public_url = "https://llmtrace.example.com"
deployment = "production"

[proxy]
default_upstream = "https://api.openai.com"
allow_upstreams = ["api.openai.com"]
max_request_body_bytes = 67108864
max_websocket_message_bytes = 16777216
max_websocket_session_bytes = 536870912

[storage]
acquire_timeout_secs = 30
retention_days = 30
retention_prune_interval_secs = 3600
retention_prune_batch_size = 1000

[auth]
cookie_secure = true
session_ttl_hours = 24

[auth.login_rate_limit]
enabled = true
max_failures = 5
window_secs = 300
lockout_secs = 900

[auth.local_admin]
username = "admin"
password_hash = "$argon2id$v=19$m=19456,t=2,p=1$..."

[redaction]
body_redaction = "json_secrets"
```

Local admin login failures are throttled in memory per username and source IP. The default allows 5 failures in 300 seconds, then returns `429 Too Many Requests` with `Retry-After` for 900 seconds. This protects a single process and records `login_throttled` audit events, but production deployments with multiple replicas or internet exposure should also enforce rate limits at the edge.

Cookie-authenticated unsafe requests are checked for same-origin browser metadata. If a `POST`, `PUT`, `PATCH`, or `DELETE` request includes an `Origin` header, it must match `server.public_url`; when `Origin` is absent, a present `Referer` must match instead. Requests without either header are allowed so non-browser API clients and health tooling are not forced to forge browser headers.

`/api`, `/api/auth`, and `/ui` responses include `Cache-Control: no-store`, legacy no-cache headers, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, and `X-Frame-Options: DENY`. These headers are intentionally not applied to proxied upstream LLM responses.

When `storage.retention_days` is set, a background task prunes old `request_traces`, minute rollups, empty trace sessions, and UI audit events in batches of `storage.retention_prune_batch_size` every `storage.retention_prune_interval_secs` seconds. Expired UI sessions and OAuth states are also cleaned up by the same task. Development mode leaves `retention_days` unset by default, so local data is not pruned unless you opt in.

Useful environment overrides include:

- Core: `LLMTRACE_DEPLOYMENT`, `LLMTRACE_PUBLIC_URL`, and `LLMTRACE_LISTEN`.
- Storage: `DATABASE_URL`, `LLMTRACE_STORAGE_MAX_CONNECTIONS`, `LLMTRACE_DB_ACQUIRE_TIMEOUT_SECS`, `LLMTRACE_TRACE_QUEUE_CAPACITY`, `LLMTRACE_TRACE_WORKER_COUNT`, `LLMTRACE_RETENTION_DAYS`, `LLMTRACE_RETENTION_PRUNE_INTERVAL_SECS`, and `LLMTRACE_RETENTION_PRUNE_BATCH_SIZE`.
- Proxy: `LLMTRACE_DEFAULT_UPSTREAM`, `LLMTRACE_ALLOW_UPSTREAMS`, `LLMTRACE_PROXY_TIMEOUT_SECS`, `LLMTRACE_MAX_BODY_CAPTURE_BYTES`, `LLMTRACE_MAX_REQUEST_BODY_BYTES`, `LLMTRACE_MAX_WEBSOCKET_MESSAGE_BYTES`, and `LLMTRACE_MAX_WEBSOCKET_SESSION_BYTES`. `LLMTRACE_ALLOW_UPSTREAMS` is a comma-separated list using the same syntax as `proxy.allow_upstreams`.
- Auth: `LLMTRACE_AUTH_COOKIE_SECURE`, `LLMTRACE_LOGIN_RATE_LIMIT_ENABLED`, `LLMTRACE_LOGIN_RATE_LIMIT_MAX_FAILURES`, `LLMTRACE_LOGIN_RATE_LIMIT_WINDOW_SECS`, `LLMTRACE_LOGIN_RATE_LIMIT_LOCKOUT_SECS`, `LLMTRACE_LOGIN_RATE_LIMIT_MAX_TRACKED_ENTRIES`, `LLMTRACE_ADMIN_USERNAME`, `LLMTRACE_ADMIN_PASSWORD`, and `LLMTRACE_ADMIN_PASSWORD_HASH`.
- Redaction: `LLMTRACE_BODY_REDACTION`, with one of `disabled`, `drop`, or `json_secrets`.

## Upstream allowlist

`proxy.allow_upstreams` accepts exact host entries, host entries with a port, URL origins, and URL path prefixes:

```toml
allow_upstreams = [
  "api.openai.com",
  "api.openai.com:443",
  "https://api.anthropic.com",
  "https://api.example.com/v1"
]
```

Host entries match the host across supported upstream schemes and paths. `host:port` entries also constrain the effective port, so `api.openai.com:443` matches `https://api.openai.com/...` but not `http://api.openai.com/...`. URL origins match only the same scheme, host, and effective port. URL path prefixes also require a path boundary, so `https://api.example.com/v1` matches `/v1` and `/v1/chat`, but not `/v10/chat`.

Allowlist entries do not support wildcards. URL entries must not contain credentials, query strings, or fragments.

## API pagination

List endpoints clamp `limit` to the range `1..=500`. `GET /api/sessions/{id}` returns session metadata plus a bounded page of messages. Use `messages_limit` and `messages_offset` to page through long sessions:

```bash
curl 'http://127.0.0.1:3000/api/sessions/<session-id>?messages_limit=100&messages_offset=0'
```

`messages_limit` defaults to `100` and is capped at `500`; `messages_offset` defaults to `0` and is capped at `1000000`. The response includes `messages_page.has_more` and `messages_page.next_offset` so clients can request the next page without assuming all messages were returned.

Authenticated read APIs run inside read-only database transactions with a local 5 second statement timeout. This applies to stats, request/session list and detail endpoints, and structured `/api/query` calls, so slow analytics reads fail without blocking write paths indefinitely.

## Runtime stats

`GET /api/stats` includes a `runtime` object with process-local background health counters. `runtime.trace_pipeline` reports enqueued, persisted, dropped, build-failed, and persist-failed trace events, plus the bounded queue capacity, available slots, and current depth. `runtime.retention` reports retention prune runs, failures, last success/failure timestamps, the last error, and the rows deleted by the most recent successful prune. These metrics reset on process restart and should be paired with logs or external metrics for long-term monitoring.

`GET /metrics` exposes the same low-sensitivity runtime counters in Prometheus text format without authentication, plus trace queue depth/capacity and database pool size/idle gauges. It intentionally avoids request URLs, headers, body content, user identifiers, API key hashes, and plugin metadata. Protect this endpoint at the network layer if deployment policy requires authenticated metrics scraping.

## Proxying examples

OpenAI-compatible base URL mode:

```bash
curl http://127.0.0.1:3000/v1/chat/completions \
  -H 'authorization: Bearer sk-example' \
  -H 'content-type: application/json' \
  -d '{"model":"gpt-4o-mini","messages":[{"role":"user","content":"hello"}]}'
```

Per-request upstream override:

```bash
curl http://127.0.0.1:3000/v1/messages \
  -H 'x-llmtrace-upstream: https://api.anthropic.com' \
  -H 'x-api-key: sk-ant-example' \
  -H 'content-type: application/json' \
  -d '{"model":"claude-3-5-sonnet-latest","max_tokens":64,"messages":[{"role":"user","content":"hello"}]}'
```

Persisted proxy outcomes include an `x-llmtrace-trace-id` response header so clients and operators can correlate a response with the stored `request_traces.id`. The header is added to completed HTTP proxy responses, request body limit rejections, upstream send failures, and accepted WebSocket upgrade responses; setup failures that are not persisted do not claim a trace ID.

## Current v1 boundaries

- UI authentication is login-only: local admin and optional OAuth/OIDC. There is no authorization or role model.
- Authenticated UI/API JSON request bodies, including login and structured query payloads, are explicitly capped at 256 KiB. Proxied traffic uses the separate `proxy.max_request_body_bytes` limit.
- Request/response bodies are stored unredacted by default. Set `redaction.body_redaction` to `drop` to store no body, or `json_secrets` to mask secret-looking JSON string values. Credential-like headers are redacted, and hashed when `redaction.store_header_hash` is enabled.
- HTTP request and response bodies are streamed through the proxy. Only the first `proxy.max_body_capture_bytes` bytes are retained for trace parsing and storage, and stored WebSocket frame transcripts are bounded by the same capture limit. Detailed trace reads also enforce that limit while decompressing stored bodies, so malformed or unexpectedly large compressed data is rejected instead of expanded without bound. Live HTTP request bodies are capped by `proxy.max_request_body_bytes`; oversized requests are rejected with `413 Payload Too Large`. WebSocket upstream connection attempts are bounded by `proxy.timeout_secs`; messages and per-direction session bytes are capped by `proxy.max_websocket_message_bytes` and `proxy.max_websocket_session_bytes`. WebSocket traces finish when either peer closes or errors, and the other half of the tunnel is dropped instead of waiting indefinitely.
- Trace enrichment, compression, and Postgres writes run in a bounded background pipeline: at most `storage.trace_queue_capacity` events wait in the queue and at most `storage.trace_worker_count` events are processed concurrently. If the queue is full, the proxy drops trace events instead of delaying live traffic.
- WASM plugins use a small JSON ABI. They enrich traces asynchronously and cannot mutate live traffic. Each invocation is bounded by the plugin's `timeout_ms` and trapped if it runs longer.
- Ad hoc analytics use a structured `/api/query` endpoint over allowlisted datasets, fields, filters, and sort keys. The legacy raw SQL endpoint is disabled.

## WASM plugin custom fields

WASM hook functions receive a JSON `HookInput` and may return a JSON object with trace enrichment fields. Custom fields can be returned as either `custom_fields` or the older `metadata` field:

```json
{
  "custom_fields": {
    "customer_tier": "enterprise",
    "billing": {
      "plan": "annual"
    }
  },
  "tags": ["paid"],
  "session_key": "tenant-a:user-123",
  "user_id": "user-123",
  "user_name": "Ada"
}
```

The recorder stores these fields under the plugin name in `request_traces.plugin_metadata`. For a plugin named `api-key-user-mapper`, the example above is persisted as:

```json
{
  "api-key-user-mapper": {
    "customer_tier": "enterprise",
    "billing": {
      "plan": "annual"
    }
  }
}
```

`/api/query` can select, filter, and order request rows by plugin metadata paths using `plugin_metadata.<plugin-name>.<field-path>`. Path segments may contain ASCII letters, digits, `_`, or `-`; avoid dots in plugin names if you want direct path queries.

```bash
curl http://127.0.0.1:3000/api/query \
  -H 'content-type: application/json' \
  -d '{
    "dataset": "requests",
    "fields": [
      "id",
      "started_at",
      "plugin_metadata.api-key-user-mapper.customer_tier",
      "plugin_metadata.api-key-user-mapper.billing.plan"
    ],
    "filters": [
      {
        "field": "plugin_metadata.api-key-user-mapper.customer_tier",
        "op": "eq",
        "value": "enterprise"
      }
    ],
    "order_by": [
      {
        "field": "plugin_metadata.api-key-user-mapper.customer_tier",
        "direction": "asc"
      }
    ],
    "limit": 50
  }'
```

Path filters compare JSON values exactly for `eq` and `ne`, use JSONB containment for `contains`, and support `is_null` and `is_not_null` for missing or JSON null fields. For broad indexed searches, querying the whole `plugin_metadata` field with `contains` can use the existing GIN index:

```json
{
  "dataset": "requests",
  "filters": [
    {
      "field": "plugin_metadata",
      "op": "contains",
      "value": {
        "api-key-user-mapper": {
          "customer_tier": "enterprise"
        }
      }
    }
  ]
}
```
