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

Validate configuration without connecting to Postgres or running migrations:

```bash
cargo run -p llmtrace -- --config llmtrace.toml --check-config
```

Proxy traffic is sent to any non-`/api` and non-`/ui` route. HTTP requests are proxied as HTTP, and requests that negotiate a standard WebSocket upgrade with `Upgrade: websocket` and a `Connection: upgrade` token are proxied as WebSocket traffic on the original path. `/ui/*` currently returns a backend placeholder until a new frontend is provided. Set `server.ui_enabled = false` to make both `/` and `/ui/*` return `404`.

## Health and readiness probes

The service exposes unauthenticated probes for production schedulers and load balancers:

- `GET /healthz` returns process liveness and does not touch external dependencies.
- `GET /readyz` verifies PostgreSQL connectivity with a lightweight `SELECT 1`. The storage check is bounded to 2 seconds and returns `503 Service Unavailable` when storage is unavailable or too slow.

The Docker image includes a `HEALTHCHECK` against `/readyz`, and `docker-compose.yml` waits for Postgres `pg_isready` before starting `llmtrace`. When running in a container, bind the service to `0.0.0.0:3000`; the compose file sets `LLMTRACE_LISTEN` for that. The included compose file is a development stack: it mounts `llmtrace.example.toml` and uses local database credentials, so it publishes llmtrace and Postgres only on the Docker host loopback interface by default.

The runtime image runs as the unprivileged `llmtrace` user with UID/GID `10001`.

Docker image builds use the committed `Cargo.lock` with `cargo build --locked` so dependency resolution is reproducible. The repository `.dockerignore` excludes local configs, `.env` files, logs, spool data, build output, and local plugin binaries from the image build context.

## Production mode

Set `server.deployment = "production"` to make startup fail fast on insecure or ambiguous settings. Production mode allows `http` or `https` for `server.public_url`, proxy upstream URLs, allowlist URL entries, and OAuth endpoints so deployments can run behind TLS-terminating reverse proxies or on private HTTP endpoints. It currently requires:

- `proxy.allow_upstreams` is non-empty, so the service cannot run as an unrestricted open proxy.
- `storage.retention_days` is set, so trace and audit storage growth is bounded.
- `auth.cookie_secure = true` when `server.public_url` uses `https`; HTTP public URLs may set it to `false` for private or reverse-proxy deployments that intentionally terminate without browser-facing HTTPS.
- `auth.login_rate_limit.enabled = true`.
- `auth.local_admin.password_hash` is set, and plaintext `auth.local_admin.password` is not set.
- `observability.metrics_bearer_token` is set, so `GET /metrics` requires `Authorization: Bearer <token>`.
- `redaction.body_redaction` is `drop` or `json_secrets`.
- When OAuth is enabled, `auth.oauth.require_email_verified = true`, and `auth.oauth.allowed_emails` or `auth.oauth.allowed_domains` is configured. `auth.oauth.issuer_url`, any explicit `auth.oauth.redirect_url`, and discovered or fallback OAuth authorization/token/userinfo endpoints may use `http` or `https`, which supports internal providers and callback URLs reached through a reverse proxy; they are still rejected if they are not valid HTTP(S) URLs or contain embedded credentials. OAuth email allowlist entries must be exact email addresses, and domain entries must be domain names such as `example.com`; URL syntax, wildcards, whitespace, and non-ASCII forms are rejected at startup.

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

[observability]
metrics_bearer_token = "replace-with-long-random-token"

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

Login usernames are capped at 320 bytes and passwords at 4096 bytes before local password verification. Oversized login payloads are treated as failed attempts and count toward the same throttle, while stored session and audit identity fields are capped at 1024 bytes.

Local password hash verification runs on Tokio's blocking worker pool so Argon2 work does not occupy async request workers.

Auth/session database operations use a 5 second application-level timeout, so slow login, logout, OAuth state, session lookup, and audit writes fail without tying up request handlers indefinitely.

`GET /api/audit-events` returns authenticated UI/auth audit events for operational review. Results are ordered newest-first and paged with `limit` and `offset`, both clamped to safe ranges; `event_type` and `user_id` filters match exact values. The response includes `page.has_more` and `page.next_offset` for follow-up requests:

```bash
curl 'http://127.0.0.1:3000/api/audit-events?event_type=login_failed&limit=100&offset=0'
```

OAuth login state is stored server-side and bound to a short-lived HttpOnly `SameSite=Lax` browser cookie scoped to `/api/auth/oauth`. The callback requires both the query `state` and cookie state to match, which keeps OAuth callbacks tied to the browser that started the login flow.

Session and OAuth state cookies must be fixed-length base64url tokens generated by llmtrace. Cookie parsing handles split `Cookie` headers from HTTP/2 or reverse proxies, and malformed or oversized token values are ignored before any database lookup.

OAuth callback authorization codes must be non-empty and at most 4096 bytes before they are sent to the provider. OAuth discovery, token, and userinfo JSON responses are capped at 64 KiB while streaming from the provider. Oversized or invalid provider responses fail the login attempt without exposing provider response details to the browser.

OAuth provider discovery, token exchange, userinfo requests, and streamed JSON response reads use `auth.oauth.timeout_secs`, which defaults to 15 seconds. This timeout is independent of the proxy upstream timeout so slow identity providers do not tie up login handlers for the full proxy request window.

Cookie-authenticated unsafe requests are checked for same-origin browser metadata. If a `POST`, `PUT`, `PATCH`, or `DELETE` request includes an `Origin` header, it must match `server.public_url`; when `Origin` is absent, a present `Referer` must match instead. Requests without either header are allowed so non-browser API clients and health tooling are not forced to forge browser headers.

`/api`, `/api/auth`, and `/ui` responses include `Cache-Control: no-store`, legacy no-cache headers, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, and `X-Frame-Options: DENY`. These headers are intentionally not applied to proxied upstream LLM responses.

When `storage.retention_days` is set, a background task prunes old `request_traces`, minute rollups, empty trace sessions, and UI audit events in batches of `storage.retention_prune_batch_size` every `storage.retention_prune_interval_secs` seconds. Expired UI sessions and OAuth states are also cleaned up by the same task. Development mode leaves `retention_days` unset by default, so local data is not pruned unless you opt in.

Useful environment overrides include:

- Core: `LLMTRACE_DEPLOYMENT`, `LLMTRACE_PUBLIC_URL`, `LLMTRACE_LISTEN`, and `LLMTRACE_UI_ENABLED`.
- Storage: `DATABASE_URL`, `LLMTRACE_STORAGE_MAX_CONNECTIONS`, `LLMTRACE_DB_ACQUIRE_TIMEOUT_SECS`, `LLMTRACE_TRACE_QUEUE_CAPACITY`, `LLMTRACE_TRACE_WORKER_COUNT`, `LLMTRACE_RETENTION_DAYS`, `LLMTRACE_RETENTION_PRUNE_INTERVAL_SECS`, and `LLMTRACE_RETENTION_PRUNE_BATCH_SIZE`.
- Proxy: `LLMTRACE_DEFAULT_UPSTREAM`, `LLMTRACE_ALLOW_UPSTREAMS`, `LLMTRACE_UPSTREAM_HEADER`, `LLMTRACE_PROXY_TIMEOUT_SECS`, `LLMTRACE_MAX_BODY_CAPTURE_BYTES`, `LLMTRACE_MAX_REQUEST_BODY_BYTES`, `LLMTRACE_MAX_WEBSOCKET_MESSAGE_BYTES`, and `LLMTRACE_MAX_WEBSOCKET_SESSION_BYTES`. `LLMTRACE_ALLOW_UPSTREAMS` is a comma-separated list using the same syntax as `proxy.allow_upstreams`.
- Auth: `LLMTRACE_AUTH_COOKIE_SECURE`, `LLMTRACE_SESSION_TTL_HOURS`, `LLMTRACE_LOGIN_RATE_LIMIT_ENABLED`, `LLMTRACE_LOGIN_RATE_LIMIT_MAX_FAILURES`, `LLMTRACE_LOGIN_RATE_LIMIT_WINDOW_SECS`, `LLMTRACE_LOGIN_RATE_LIMIT_LOCKOUT_SECS`, `LLMTRACE_LOGIN_RATE_LIMIT_MAX_TRACKED_ENTRIES`, `LLMTRACE_ADMIN_USERNAME`, `LLMTRACE_ADMIN_PASSWORD`, and `LLMTRACE_ADMIN_PASSWORD_HASH`.
- OAuth: `LLMTRACE_OAUTH_ENABLED`, `LLMTRACE_OAUTH_ISSUER_URL`, `LLMTRACE_OAUTH_CLIENT_ID`, `LLMTRACE_OAUTH_CLIENT_SECRET`, `LLMTRACE_OAUTH_REDIRECT_URL`, `LLMTRACE_OAUTH_TIMEOUT_SECS`, `LLMTRACE_OAUTH_REQUIRE_EMAIL_VERIFIED`, `LLMTRACE_OAUTH_ALLOWED_EMAILS`, and `LLMTRACE_OAUTH_ALLOWED_DOMAINS`. The allowed email/domain variables are comma-separated lists and use the same validation as the toml fields.
- Observability: `LLMTRACE_METRICS_BEARER_TOKEN`, which requires `Authorization: Bearer <token>` on `GET /metrics` when set and is required in production mode.
- Redaction: `LLMTRACE_SENSITIVE_HEADERS`, `LLMTRACE_STORE_HEADER_HASH`, and `LLMTRACE_BODY_REDACTION`, with `LLMTRACE_BODY_REDACTION` set to one of `disabled`, `drop`, or `json_secrets`.

Startup validation rejects obviously dangerous resource limits before the service binds a port. Proxy timeout is capped at 3600 seconds; stored body capture at 64 MiB; live HTTP request bodies at 1 GiB; WebSocket messages at 64 MiB and sessions at 2 GiB. Storage pools are capped at 1024 connections, trace queues at 1000000 entries, trace workers at 128, DB acquire timeout at 300 seconds, and retention prune intervals at 86400 seconds. Login throttling is capped at 1000 failures, 86400 second windows/lockouts, and 1000000 tracked entries. OAuth provider timeout is capped at 300 seconds.

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

Allowlist entries do not support wildcards. URL entries must not contain credentials, query strings, or fragments. Runtime upstream URLs from `proxy.default_upstream`, absolute-form request URIs, or the configured upstream override header must also omit embedded credentials. HTTP proxying accepts only `http` and `https` upstream URLs; WebSocket proxying accepts only `ws` and `wss` after HTTP(S) URLs are converted for the upgrade path.

## API pagination

List endpoints clamp `limit` to the range `1..=500`. `GET /api/requests?q=...` trims empty search strings, rejects search terms over 512 bytes, and treats `%` and `_` as literal characters instead of SQL wildcard operators. `GET /api/sessions/{id}` returns session metadata plus a bounded page of messages. Use `messages_limit` and `messages_offset` to page through long sessions:

```bash
curl 'http://127.0.0.1:3000/api/sessions/<session-id>?messages_limit=100&messages_offset=0'
```

`messages_limit` defaults to `100` and is capped at `500`; `messages_offset` defaults to `0` and is capped at `1000000`. The response includes `messages_page.has_more` and `messages_page.next_offset` so clients can request the next page without assuming all messages were returned.

Authenticated read APIs run inside read-only database transactions with a local 5 second statement timeout. This applies to stats, request/session list and detail endpoints, and structured `/api/query` calls, so slow analytics reads fail without blocking write paths indefinitely.

Structured `/api/query` requests are also bounded before SQL construction: at most 64 selected fields, 32 filters, 8 sort keys, 4 KiB per string filter value, and 16 KiB per JSON filter value.

`GET /api/query/schema` returns the structured query catalog for clients and UI builders: datasets, default fields, default sort order, field filter kinds, supported operators, sort directions, query limits, and plugin metadata path constraints. Treat this response as the source of truth for generated query builders instead of hard-coding field lists from the README.

Use `POST /api/query/export.jsonl` with the same structured query payload to download the selected rows as newline-delimited JSON. The export reuses the same dataset, field, filter, sort, limit, read-only transaction, and statement-timeout rules as `/api/query`; responses use `application/x-ndjson`, include `Content-Disposition: attachment`, and expose the row count in `x-llmtrace-export-rows`.

```bash
curl http://127.0.0.1:3000/api/query/export.jsonl \
  -H 'content-type: application/json' \
  -o llmtrace-requests.jsonl \
  -d '{
    "dataset": "requests",
    "fields": ["id", "started_at", "upstream_host", "status", "model", "duration_ms"],
    "filters": [{"field": "status", "op": "gte", "value": 500}],
    "order_by": [{"field": "started_at", "direction": "desc"}],
    "limit": 500
  }'
```

## Runtime stats

`GET /api/stats` includes a `runtime` object with process-local background health counters. `runtime.trace_pipeline` reports enqueued, persisted, dropped, build-failed, and persist-failed trace events, plus the bounded queue capacity, available slots, and current depth. `runtime.retention` reports retention prune runs, failures, last success/failure timestamps, the last error, and the rows deleted by the most recent successful prune. These metrics reset on process restart and should be paired with logs or external metrics for long-term monitoring.

`GET /api/usage/summary?since_hours=24&limit=10` returns dashboard-oriented traffic aggregates over a bounded lookback window: totals, top models, top upstream hosts, status classes, and request kinds. `since_hours` defaults to 24 and is capped at 2160 hours; `limit` defaults to 10 and is capped at 50.

`GET /api/usage/timeseries?since_hours=24&bucket=hour` returns continuous rollup-backed time buckets for traffic charts. Buckets may be `minute`, `hour`, or `day`; minute buckets cap the lookback at 24 hours, hour buckets at 2160 hours, and day buckets at 8760 hours. Empty buckets are returned with zero counts so clients can render stable charts without filling gaps themselves.

`GET /metrics` exposes the same low-sensitivity runtime counters in Prometheus text format, plus trace queue depth/capacity and database pool size/idle gauges. It intentionally avoids request URLs, headers, body content, user identifiers, API key hashes, and plugin metadata. Development mode may leave this endpoint unauthenticated for simple Prometheus scraping; production mode requires `observability.metrics_bearer_token` or `LLMTRACE_METRICS_BEARER_TOKEN`, and requests must send `Authorization: Bearer <token>`. Keep network-layer protections when deployment policy requires them.

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

The upstream override header is a control header and must appear at most once per request. Requests with malformed or repeated override headers are rejected with `400`, and upstreams outside `proxy.allow_upstreams` are rejected with `403` instead of being reported as upstream outages.

Persisted proxy outcomes include an `x-llmtrace-trace-id` response header so clients and operators can correlate a response with the stored `request_traces.id`. The header is added to completed HTTP proxy responses, request body limit rejections, upstream send failures, and accepted WebSocket upgrade responses; setup failures that are not persisted do not claim a trace ID.

The proxy does not forward the inbound `Host`, `Cookie`, or `x-llmtrace-*` control headers to upstreams. Standard hop-by-hop headers and any extension headers named by `Connection` are stripped on both request and response forwarding, including WebSocket upstream handshakes. Upstream HTTP and WebSocket clients derive `Host` from the resolved upstream URL, which avoids leaking the llmtrace listener host, llmtrace UI session cookies, or honoring user-supplied virtual-host overrides. Proxied upstream `Set-Cookie` responses are also stripped so upstreams cannot set cookies on the llmtrace origin; use authorization or API-key headers for upstream authentication.

## Current v1 boundaries

- UI authentication is login-only: local admin and optional OAuth/OIDC. There is no authorization or role model.
- Authenticated UI/API JSON request bodies, including login and structured query payloads, are explicitly capped at 256 KiB. Proxied traffic uses the separate `proxy.max_request_body_bytes` limit.
- Request/response bodies are stored unredacted by default. Set `redaction.body_redaction` to `drop` to store no body, or `json_secrets` to mask secret-looking JSON string values. In `json_secrets` mode, malformed or truncated bodies that look like JSON objects or arrays are dropped instead of being stored unredacted. Credential-like HTTP and WebSocket handshake headers, including `Cookie`, `Set-Cookie`, `Authorization`, and header names containing `token`, `secret`, `password`, `api-key`, or `apikey`, are redacted and hashed when `redaction.store_header_hash` is enabled. Persisted request/upstream URIs keep paths and query parameter names, but embedded credentials are stripped and all query values are replaced with `REDACTED`; the configured upstream override header receives the same URL query-value redaction, and request logs record only the path.
- HTTP request and response bodies are streamed through the proxy. Only the first `proxy.max_body_capture_bytes` bytes are retained for trace parsing and storage, and stored WebSocket frame transcripts are bounded by the same capture limit. Detailed trace reads also enforce that limit while decompressing stored bodies, so malformed or unexpectedly large compressed data is rejected instead of expanded without bound. Live HTTP request bodies are capped by `proxy.max_request_body_bytes`; oversized requests are rejected with `413 Payload Too Large`. WebSocket upstream connection attempts are bounded by `proxy.timeout_secs`; messages and per-direction session bytes are capped by `proxy.max_websocket_message_bytes` and `proxy.max_websocket_session_bytes`. WebSocket traces finish when either peer closes or errors, and the other half of the tunnel is dropped instead of waiting indefinitely.
- Parsed session messages derived from captured bodies are capped before persistence at 128 messages per trace, 64 bytes per role, and 16 KiB per message content. Truncated message captures add the `session_messages_truncated` trace tag.
- Trace enrichment, compression, and Postgres writes run in a bounded background pipeline: at most `storage.trace_queue_capacity` events wait in the queue and at most `storage.trace_worker_count` events are processed concurrently. If the queue is full, the proxy drops trace events instead of delaying live traffic.
- WASM plugins use a small JSON ABI. They enrich traces asynchronously and cannot mutate live traffic. Startup validation allows at most 64 plugins; plugin names must be printable ASCII and at most 128 bytes; duplicate hooks in one plugin are rejected. Each invocation is bounded by the plugin's `timeout_ms` and trapped if it runs longer. Hook outputs are capped at 64 KiB before decoding so a faulty plugin cannot force unbounded allocations in the trace worker. Plugin-derived session keys, user identifiers, display names, and tags are bounded again before persistence; truncated enrichment adds the `trace_enrichment_truncated` tag.
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

`/api/query` can select, filter, and order request rows by plugin metadata paths using `plugin_metadata.<plugin-name>.<field-path>`. Path segments may contain ASCII letters, digits, `_`, or `-`; use those characters in plugin names if you want direct path queries.

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

Invalid structured query definitions return `400` with a validation message. Database execution failures, statement timeouts, and other internal query errors are logged server-side and return a generic `500` response so storage details are not exposed to API clients.

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
