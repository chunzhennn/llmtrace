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
- `auth.cookie_secure = true`.
- `auth.login_rate_limit.enabled = true`.
- `auth.local_admin.password_hash` is set, and plaintext `auth.local_admin.password` is not set.
- `redaction.body_redaction` is `drop` or `json_secrets`.
- When OAuth is enabled, `auth.oauth.allowed_emails` or `auth.oauth.allowed_domains` is configured.

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

Useful environment overrides include `LLMTRACE_DEPLOYMENT`, `LLMTRACE_PUBLIC_URL`, `LLMTRACE_LISTEN`, `DATABASE_URL`, `LLMTRACE_DEFAULT_UPSTREAM`, `LLMTRACE_MAX_REQUEST_BODY_BYTES`, `LLMTRACE_MAX_WEBSOCKET_MESSAGE_BYTES`, `LLMTRACE_MAX_WEBSOCKET_SESSION_BYTES`, `LLMTRACE_AUTH_COOKIE_SECURE`, `LLMTRACE_LOGIN_RATE_LIMIT_ENABLED`, `LLMTRACE_LOGIN_RATE_LIMIT_MAX_FAILURES`, `LLMTRACE_LOGIN_RATE_LIMIT_WINDOW_SECS`, `LLMTRACE_LOGIN_RATE_LIMIT_LOCKOUT_SECS`, `LLMTRACE_LOGIN_RATE_LIMIT_MAX_TRACKED_ENTRIES`, `LLMTRACE_ADMIN_USERNAME`, `LLMTRACE_ADMIN_PASSWORD`, and `LLMTRACE_ADMIN_PASSWORD_HASH`.

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

## Current v1 boundaries

- UI authentication is login-only: local admin and optional OAuth/OIDC. There is no authorization or role model.
- Request/response bodies are stored unredacted by default. Set `redaction.body_redaction` to `drop` to store no body, or `json_secrets` to mask secret-looking JSON string values. Credential-like headers are redacted, and hashed when `redaction.store_header_hash` is enabled.
- HTTP request and response bodies are streamed through the proxy. Only the first `proxy.max_body_capture_bytes` bytes are retained for trace parsing and storage. Detailed trace reads also enforce that limit while decompressing stored bodies, so malformed or unexpectedly large compressed data is rejected instead of expanded without bound. Live HTTP request bodies are capped by `proxy.max_request_body_bytes`; oversized requests are rejected with `413 Payload Too Large`. WebSocket messages and per-direction session bytes are capped by `proxy.max_websocket_message_bytes` and `proxy.max_websocket_session_bytes`.
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
