# LiteLLM gateway and separate admin domain

Run one llmtrace process with two listeners. Users reach LiteLLM through the
proxy listener; administrators reach the llmtrace SPA and authenticated analytics
on the admin listener. Existing configurations without `admin_listen` retain the
combined listener.

```text
api.example.com   -> ingress -> llmtrace:3000 -> LiteLLM:4000
trace.example.com -> ingress -> llmtrace:3001 -> admin UI, auth, analytics, probes
```

Copy `llmtrace.litellm.example.toml` to your private `llmtrace.toml` for a local
setup. For an enterprise cluster, use the following routing settings alongside
the production authentication, retention and metrics settings in README:

```toml
[server]
listen = "0.0.0.0:3000"
admin_listen = "0.0.0.0:3001"
public_url = "https://trace.example.com"
proxy_public_url = "https://api.example.com"
deployment = "production"

[proxy]
preset = "litellm"
default_upstream = "http://litellm:4000"
allow_upstreams = ["http://litellm:4000", "ws://litellm:4000"]
```

`public_url` is the **admin** origin. Login origin checks, OAuth's default callback
(`/api/auth/oauth/callback`), redirects and secure cookies use this URL. The SPA
uses relative `/api` URLs on its own domain; no cross-domain admin CORS is needed.
Both public URLs must be origins without paths or credentials and must have
different hostnames: using different ports alone does not isolate browser cookies.
`admin_listen` and `proxy_public_url` must be set together. Public URLs describe
the externally reachable endpoints; configure DNS, certificates and ingress
routing separately. Listener identity determines routing, not `Host` or
`X-Forwarded-Host`. The proxy listener never serves llmtrace admin routes, even
with a forged admin hostname. The admin listener never forwards traffic to LiteLLM.

LiteLLM remains responsible for user authentication, quotas and endpoint
authorization. Clients send their existing LiteLLM keys unchanged and can use
`https://api.example.com/v1` as their SDK base URL. The preset does not inject
provider credentials or automatically install an identity lookup plugin.
URL allowlist entries match schemes exactly: include `ws://` alongside `http://`
(or `wss://` alongside `https://`) for WebSocket traffic, as in the examples.

## Preset routes and capture

LiteLLM supports [unversioned API routes and several SDK formats](https://docs.litellm.ai/docs/proxy/user_keys),
[health endpoints](https://docs.litellm.ai/docs/proxy/health), and
[custom pass-through routes](https://docs.litellm.ai/docs/proxy/pass_through).
The built-in preset covers common user APIs:

| Traffic | Forwarding | Archive/session capture |
| --- | --- | --- |
| `/chat/completions`, `/completions`, `/responses`, `/messages`, `/embeddings`, `/images`, `/audio`, `/moderations`, `/rerank`, `/realtime` and their `/v1` equivalents | Allowed, including descendants | POST requests and WebSocket upgrades |
| `/models`, `/files`, `/batches`, `/assistants`, `/threads`, `/fine_tuning`, `/model/info`, `/key/info`, `/user/info`, `/health` | Allowed, including descendants | None |
| Other `/v1` routes | Allowed for compatibility | None |
| Other paths, including `/ui`, `/key/generate`, `/config` | Local 404 | None |

Captured traffic keeps the existing bounded asynchronous pipeline. Protocol-specific
parsing is richest for chat completions, Responses and Messages; forwarding an
endpoint does not imply its multipart body, usage or cost can be fully parsed.
Batch/assistant execution is not correlated into inference sessions by this preset.
Supporting traffic skips body copies, plugins, journal writes and conversation
records, so model discovery and health checks do not inflate inference statistics.
It has no persisted per-request analytics or llmtrace trace ID. HTTP upload limits,
upstream allowlists, timeouts and streaming still apply. OPTIONS requests pass
upstream so LiteLLM remains responsible for user-facing CORS.

`proxy.path_prefixes` replaces the preset's forwarding list; it never changes the
destination allowlist. `proxy.capture_path_prefixes` independently replaces its
capture list and captures **all methods** on matching forwarded paths. `[]`
disables capture. Prefixes match path segment boundaries. For provider-specific
routes or custom LiteLLM pass-through endpoints, explicitly extend both lists as
needed. For a transparent proxy of the entire LiteLLM application, set:

```toml
[proxy]
preset = "litellm"
path_prefixes = ["/"]
```

This exposes LiteLLM's own UI/management routes, protected by LiteLLM's permissions;
they receive no conversation capture by default. In split mode `/ui`, `/api`,
`/healthz` and `/metrics` on the user listener belong to the upstream. The
llmtrace probes and metrics are on the **admin listener**. Restrict that listener
through your internal ingress/network policy. A root prefix in legacy combined
mode still leaves llmtrace's reserved paths local.

In the LiteLLM preset, `x-llmtrace-upstream` selects an allowed **base URL** and
preserves the incoming route. It cannot replace `/models` with an inference
endpoint to bypass capture. Dot segments, backslashes and encoded path separators
are rejected with 400 before forwarding so path normalization cannot evade routing
or capture rules. Ordinary encoded identifiers remain supported.

## Environment variables and containers

| Environment variable | Configuration |
| --- | --- |
| `LLMTRACE_LISTEN` | `server.listen` (proxy, or combined in legacy mode) |
| `LLMTRACE_ADMIN_LISTEN` | `server.admin_listen` |
| `LLMTRACE_PUBLIC_URL` | `server.public_url` (admin) |
| `LLMTRACE_PROXY_PUBLIC_URL` | `server.proxy_public_url` |
| `LLMTRACE_PROXY_PRESET` | `proxy.preset`: `custom` or `litellm` |
| `LLMTRACE_DEFAULT_UPSTREAM` | `proxy.default_upstream` |
| `LLMTRACE_ALLOW_UPSTREAMS` | Comma-separated destination allowlist |
| `LLMTRACE_PROXY_PATH_PREFIXES` | Comma-separated forwarding prefixes |
| `LLMTRACE_CAPTURE_PATH_PREFIXES` | Comma-separated capture prefixes; empty disables |

Environment overrides file values. Explicit prefix lists override preset defaults,
including when the preset is selected through the environment. Changes require a
restart. `--check-config` validates the resolved settings without a database.

For containers bind both listeners to `0.0.0.0`, expose ports 3000 and 3001, and
route each ingress hostname to the appropriate port without rewriting paths.
Enable WebSocket upgrades and disable response buffering on the proxy ingress;
set its timeout to accommodate long LLM streams. Probe `http://127.0.0.1:3001/readyz`
instead of port 3000. The Docker image accepts `LLMTRACE_HEALTHCHECK_URL` to change
its health probe target; Compose users must also override its explicit healthcheck.
Both listeners stop accepting connections on SIGTERM, then drain in-flight HTTP
requests before shutting down the trace pipeline.

## Validation

The local backend suite passed 374 tests, including isolated PostgreSQL tests:

```sh
DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/llmtrace \
  cargo test --workspace --locked -- --include-ignored \
  --skip performance_ --skip journal_process_helper --skip openrouter_live
cargo clippy --workspace --all-targets --locked -- -D warnings
corepack pnpm --dir crates/llmtrace/ui check
corepack pnpm --dir crates/llmtrace/ui test
corepack pnpm --dir crates/llmtrace/ui build
cargo build --release --locked -p llmtrace
```

The frontend passed 43 tests and Svelte diagnostics reported no errors or warnings.
The routing integration test uses synthetic HTTP/SSE/WebSocket traffic and verifies
user-key forwarding, CORS, upstream errors, upload limits, progressive downloads,
capture selection, origin checks and admin isolation with both preset and root
forwarding prefixes. It requires no running LiteLLM instance or provider credentials.

An additional release-process smoke test verified that a failed admin bind releases
the proxy socket and that SIGTERM drains an active supporting download before both
listeners exit. Five hundred supporting requests at concurrency 16 produced no
persisted traces. The [smoke measurements](ui-review/split-routing/smoke.json) include
fresh client connections and a Python upstream; they are a local sanity check,
not an isolated proxy-overhead measurement or production capacity estimate.

The [browser report](ui-review/split-routing/report.json) passed seven checks across
1440, 390 and 320 pixel widths, including login, expandable route lists, no page
overflow, admin API origin isolation and absence of the admin cookie on the proxy
hostname. See the [System configuration screenshot](ui-review/split-routing/desktop-routing.png).
No paid LLM calls were made for these changes. The existing OpenRouter review
instance can use split listeners independently of selecting the LiteLLM preset.
