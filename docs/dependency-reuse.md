# Reusing protocol and UI libraries

Keep provider-specific trace semantics and resource limits in this project;
delegate general protocols and widgets to maintained libraries when their APIs
fit the capture pipeline and SPA.

Selection prioritizes active maintenance and demonstrated downstream adoption.
A specialized library may have few GitHub stars; record that limitation instead
of presenting it as a large-community project. Check release history, public
API suitability, dependency cost, and protocol regressions before integrating.

## Captured SSE responses (Rust)

[sse-stream](https://github.com/4t145/sse-stream) 0.3 handles event framing,
multiline data, comments, BOMs, and LF/CRLF/CR line endings. It replaces
`sse-codec` and removes the `futures_codec` dependency.

The maintenance/adoption review on 2026-09-23 found:

- `sse-stream` 0.3.0 was published on 2026-09-18. Its repository had 11 stars;
  community size is a limitation, not the selection rationale.
- The [official MCP Rust SDK's manifest](https://github.com/modelcontextprotocol/rust-sdk/blob/main/crates/rmcp/Cargo.toml)
  uses `sse-stream` 0.2.4. That establishes adoption of the project, not production
  validation of the newer 0.3 API selected here. The latter is covered by our
  capture parsing and TTFT regression tests.
- `sse-codec` had 13 stars and its last commit was 2026-02-12;
  `eventsource-stream` had 38 stars and its last commit was 2022-02-17.
  A convenient API or high download count alone does not establish active maintenance.

`SseByteStream` consumes a borrowed `bytes::Buf` directly. A small adapter counts
the bytes actually advanced by the parser so TTFT uses each event's exact end,
even when many events share one input buffer. There is no whole-capture copy or
application line splitter. The finite in-memory source is immediately ready, so
polling it needs neither an executor nor a network client. Provider parsing stays
in the existing synchronous background pipeline. Memory remains bounded by
capture limits; protocol scratch buffers belong to the library.

Only terminated events are interpreted; an incomplete event at EOF is discarded.
The decoder stops on invalid UTF-8 in recognized fields while preserving earlier
complete events. Metadata-only blocks are ignored by the response parser.
Provider delta assembly, usage/tool limits, terminal markers, and the
byte-offset-to-timestamp mapping remain application logic. Parsing still runs
outside the live proxy path.

Regression coverage includes all three newline conventions, leading versus
embedded BOMs, multiline data, metadata/role-only events, unterminated terminal
markers, invalid UTF-8, large events, and exact TTFT byte offsets, alongside the
existing provider parsing tests.

## Transcript SSE (TypeScript)

[eventsource-parser](https://github.com/rexxars/eventsource-parser) replaces the
regular-expression block splitter and hand-written `data:` extraction. Feed
8 KiB character chunks and consume events immediately rather than allocate an
array for the whole capture. The parser handles CR/LF/CRLF, BOMs, comments, and
multiline fields in the transcript worker without a network client.

Unterminated trailing data is drained solely into an `unparsed_event` preview;
it cannot contribute deltas or mark a transcript complete, even if it contains
`[DONE]`. This aligns completion with the backend while retaining forensic text.
Provider-specific reconstruction, unknown-event display and context deduplication
remain local because they are trace audit behavior, not the SSE protocol.

## Icons

[Lucide for Svelte](https://lucide.dev/guide/svelte/getting-started) replaces the
copied SVG path catalog and `{@html}` renderer. `Icon.svelte` only maps existing
application names to statically imported components and forwards size/class.
Per-icon imports avoid loading the full catalog, and icons remain decorative
(`aria-hidden`) beside their existing accessible labels. Future icons should
come from Lucide rather than new copied path strings.

## Notifications

[svelte-sonner](https://github.com/wobsoriano/svelte-sonner) owns the notification
queue, IDs, dismissal, timer cleanup, animations, live region, and keyboard/focus
behavior. The application facade keeps the existing success/info (5 seconds)
and error (8 seconds) durations. `Toasts.svelte` only configures placement,
dismiss buttons, colors, and the application's light/dark theme. The default
three visible notifications keep bursts bounded on screen; hover/focus and page
visibility behavior come from Sonner rather than another local timer system.

## Authentication primitives

The [cookie crate](https://docs.rs/cookie/latest/cookie/) handles Cookie header
parsing and Set-Cookie serialization. The local code still enforces the exact
URL-safe token alphabet/length, accepts the first valid matching token across
multiple headers, and chooses the session/OAuth paths and lifetimes. Parsing is
not percent-decoding, and quoted tokens still fail token validation. A shared
builder applies HttpOnly, SameSite=Lax, configured Secure, and Max-Age for both
issuance and removal.

[subtle](https://docs.rs/subtle/latest/subtle/trait.ConstantTimeEq.html), already
present transitively, replaces the manual XOR equality loop. Compare SHA-256
digests of the two credential strings so the compared values have equal fixed
lengths; subtle's slice implementation otherwise short-circuits on unequal
lengths. Argon2 password verification and the rule to verify the password even
for an incorrect username are unchanged.

## Other reviewed code

- Charts and query editing already delegate to Chart.js and CodeMirror.
- The query-text parser implements a restricted application DSL, including
  allowlisted fields and translation to `/api/query`. A general SQL parser would
  still require that validation and introduce unsupported syntax expectations.
- Provider delta assembly, transcript context deduplication, and bounded JSON
  previews encode audit semantics and capture limits. Replacing them with generic
  object merging, chat SDKs, or JSON viewers would not preserve those guarantees.
- The JSONL reader preserves record-level backpressure and cancellation; the
  small resource helper preserves navigation cancellation without introducing
  cache/refetch policy. These are application adapters around browser primitives.
- Simple visual wrappers (cards, labels, tables, pagination) contain layout and
  application state binding rather than an independent widget engine. Keep them
  small; adopt a headless component library if richer interactions are needed.

No database schema, configuration, API contract, or live proxy behavior changes
are required by these replacements.

## Validation

Backend checks rerun for the `sse-stream` replacement:

- `cargo test --workspace --locked`: 311 passed; 28 existing opt-in tests ignored.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: passed.
- `cargo fmt --all -- --check`: passed.

Frontend results from the preceding UI refactor (no UI changes in this replacement):

- UI `pnpm test`: 180 passed.
- UI `pnpm run check`: no errors or warnings.
- UI `pnpm run build`: static production build passed.
- Headless Chrome with synthetic auth responses: login icons, notification
  rendering, dark/light switching, dismiss buttons, and a 390 px viewport passed
  without runtime exceptions. No real backend/session data was used.
