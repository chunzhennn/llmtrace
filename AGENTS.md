# Repository Guidelines

## Project Structure & Module Organization

This is a Rust workspace with one binary crate in `crates/llmtrace`. The root `Cargo.toml` holds workspace metadata and dependency versions; `crates/llmtrace/Cargo.toml` wires those dependencies into the application. The admin frontend is a SvelteKit SPA in `crates/llmtrace/ui` (Svelte 5 runes, Tailwind v4, `@sveltejs/adapter-static` with base path `/ui`). It is built to `crates/llmtrace/ui/build` and embedded into the binary via `rust-embed`; `crates/llmtrace/src/ui.rs` serves those assets under `/ui/*` (gated by `server.ui_enabled`) with an SPA fallback to `index.html`.

Application code lives in `crates/llmtrace/src`:

- `main.rs` loads config, runs migrations, builds the Axum router, starts graceful shutdown, and drains the trace pipeline.
- `proxy.rs` handles HTTP, SSE-style streaming responses, and WebSocket reverse proxying. It captures request/response bodies up to live audit limits while preserving streaming behavior.
- `trace.rs` owns the bounded background trace pipeline. It parses captured traffic, runs WASM plugins, appends bodies into compressed archive segments, and persists traces outside the live proxy path.
- `storage.rs` owns Postgres access, migrations, trace/session writes, rollups, and the structured `/api/query` implementation over allowlisted datasets and fields.
- `api.rs` exposes authenticated JSON API routes for stats, requests, sessions, structured queries, and plugin statuses.
- `auth.rs` implements local admin login, optional OAuth/OIDC login, session cookies, and UI audit events.
- `config.rs`, `types.rs`, `parsers.rs`, `plugins.rs`, `redaction.rs`, and `state.rs` provide configuration, shared enums, LLM trace parsing, the Wasmtime plugin ABI, sensitive-data handling, and shared application state.

Database migrations are in `crates/llmtrace/migrations` and are embedded with `sqlx::migrate!("./migrations")`. Update migrations, storage row mappings, query field allowlists, and tests together when changing persisted schema.

The frontend lives in `crates/llmtrace/ui`. Source is under `ui/src` (`lib/api` for the typed client and endpoint wrappers, `lib/components` for shared UI, `lib/state` for auth/theme/toast rune stores, `lib/utils` for formatting/query helpers, and `routes` for pages). The built output in `ui/build` is git-ignored except for a placeholder `index.html` so the `rust-embed` folder always exists; local and Docker builds overwrite it with real hashed assets. When adding API surface, keep `ui/src/lib/api/types.ts` and the endpoint wrappers in sync with the Rust JSON shapes.

## Build, Test, and Development Commands

- `docker compose up -d postgres`: start the local Postgres service.
- `cp llmtrace.example.toml llmtrace.toml`: create a local config before editing secrets, upstreams, or database URLs.
- `cargo run -p llmtrace -- --config llmtrace.toml`: run the proxy locally.
- `cargo run -p llmtrace -- --config llmtrace.toml --migrate-only`: apply embedded migrations and exit.
- `cargo check --workspace`: type-check the workspace.
- `cargo test --workspace`: run unit tests.
- `cargo fmt --all`: format Rust code with rustfmt.
- `cargo clippy --workspace --all-targets -- -D warnings`: run lint checks with warnings treated as errors.
- `docker compose up --build llmtrace`: build and run the service container with Postgres.

Frontend commands run from `crates/llmtrace/ui` (uses `pnpm`):

- `pnpm install`: install frontend dependencies.
- `pnpm run dev`: start the Vite dev server (proxies `/api`, `/healthz`, `/readyz` to `http://127.0.0.1:3000` and rewrites `Origin` so the backend same-origin check passes). Override the target with `LLMTRACE_BACKEND`.
- `pnpm run build`: build the SPA into `ui/build` (embedded by the backend). Rebuild after UI changes before running a release binary.
- `pnpm run check`: run `svelte-check` type/diagnostics.

Useful config overrides are `LLMTRACE_CONFIG`, `DATABASE_URL`, `LLMTRACE_LISTEN`, `LLMTRACE_DEFAULT_UPSTREAM`, `LLMTRACE_ADMIN_USERNAME`, `LLMTRACE_ADMIN_PASSWORD`, and `LLMTRACE_ADMIN_PASSWORD_HASH`.

## Coding Style & Design Constraints

Use Rust 2024 edition conventions and rustfmt defaults: four-space indentation, `snake_case` for functions/modules, `PascalCase` for types, and `SCREAMING_SNAKE_CASE` for constants. Follow the existing error style with `anyhow`, `thiserror`, contextual messages, and explicit API error translation.

Keep proxy latency and streaming behavior central. Do not move Postgres writes, compression, parsing, or plugin execution back onto the live request/response path. The proxy should continue to stream upstream bodies while retaining audit payloads only up to `proxy.max_request_body_bytes`, `proxy.max_response_body_bytes`, and WebSocket limits. The trace recorder intentionally uses a bounded `try_send` queue and drops trace events when saturated instead of delaying proxied traffic.

Use SQLx bind parameters and `QueryBuilder` for dynamic SQL. Public analytics must stay on the structured `/api/query` surface backed by allowlisted datasets, fields, filters, and sort keys. Do not add generic raw-SQL API endpoints. When adding queryable fields, update the relevant `FieldSpec`, default field lists if needed, filter validation, and unit tests.

Keep module boundaries narrow. Avoid broad refactors in `proxy.rs`, `storage.rs`, `auth.rs`, or `trace.rs` unless a requested change requires them. Prefer adding small helpers near the behavior they support. For schema-affecting changes, update the migration and all storage/API projections that expose the field.

## Proxy, Auth, and Plugin Notes

Non-`/api` and non-`/ui` routes fall through to the proxy only when the request path matches one of `proxy.path_prefixes`; other paths return `404` locally without upstream forwarding or trace recording. `/api/auth/*` is public for login/logout/session/OAuth flows; other `/api/*` routes are protected by `auth::require_auth`. Per-request upstream overrides use the configured `proxy.upstream_header` (`x-llmtrace-upstream` by default) and should continue to respect `proxy.allow_upstreams`. Upstream override headers do not bypass `proxy.path_prefixes`.

WASM plugins are loaded through `plugins.rs` and invoked asynchronously from `trace.rs`; they enrich traces only and must not mutate live traffic. The ABI expects exported hook functions named `llmtrace_on_request_start`, `llmtrace_on_response_headers`, and/or `llmtrace_on_response_end`, plus `memory` and `llmtrace_alloc`. Plugin output may use `custom_fields` or legacy `metadata`; persisted fields are nested under the plugin name in `request_traces.plugin_metadata`.

Queryable plugin metadata paths are supported only on the `requests` dataset using `plugin_metadata.<plugin-name>.<field-path>`. Path segments must contain only ASCII letters, digits, `_`, or `-`; avoid dots in plugin names if those fields need direct path queries.

## Testing Guidelines

Current tests are Rust unit tests colocated with implementation, especially structured query validation in `storage.rs`, archive segment helpers in `storage.rs`, and header/URI redaction behavior in `redaction.rs`. Add tests near changed code and name them after behavior, for example `structured_query_rejects_unknown_field` or `archive_frame_round_trips_body`.

For parser, query-builder, redaction, and validation changes, prefer focused unit tests that do not need Postgres. For database-sensitive work, run `docker compose up -d postgres` and verify migrations with `cargo run -p llmtrace -- --config llmtrace.toml --migrate-only`. Run `cargo test --workspace` before submitting changes; run `cargo clippy --workspace --all-targets -- -D warnings` for changes touching shared proxy, auth, storage, trace, or API behavior.

## Commit & Pull Request Guidelines

This repository has no established commit history, so use short, imperative subjects such as `Add trace query validation` and keep unrelated changes separate. Pull requests should describe the behavioral change, mention config or migration impacts, list test commands run, and include example requests or screenshots when API/UI behavior changes.

## Security & Configuration Tips

Do not commit `llmtrace.toml`, `.env`, logs, `spool/`, captured trace dumps, or local plugin binaries containing secrets. The example config uses development credentials and stores request/response bodies unredacted in archive segments for internal audit.

Stored headers redact configured credential-like header values and may keep SHA-256 hashes when `redaction.store_header_hash` is enabled. WASM plugins receive captured request/response data before persistence redaction, so plugin code and plugin metadata must be treated as sensitive. Prefer `auth.local_admin.password_hash` over plaintext `password` outside local development, set `auth.cookie_secure = true` behind HTTPS, and configure OAuth `allowed_emails` or `allowed_domains` when enabling OAuth for non-local deployments.
