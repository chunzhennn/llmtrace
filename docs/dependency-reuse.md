# Reusing protocol and UI libraries

Keep provider-specific trace semantics and resource limits in this project;
delegate general protocols and widgets to maintained libraries when their APIs
fit the capture pipeline and SPA.

## Captured SSE responses (Rust)

[sse-codec](https://github.com/goto-bus-stop/sse-codec) handles event framing,
multiline data, comments, BOMs, and LF/CRLF/CR line endings. Its synchronous
`Decoder` interface fits the existing background parser and exposes consumed
bytes for TTFT. `futures_codec` supplies its decoder trait and buffer type; it
does not introduce another executor or HTTP client. Feed through each line ending
to avoid copying the entire capture or repeatedly scanning a large partial line.
Memory remains bounded by capture limits.

Only terminated events are interpreted; an incomplete event at EOF is discarded.
Complete events before invalid UTF-8 remain available. Provider delta assembly,
usage/tool limits, terminal markers, and byte-offset-to-timestamp mapping remain
application logic. Parsing still runs outside the live proxy path.

Regression coverage includes all three newline conventions, BOMs, multiline
data, unterminated terminal markers, large UTF-8 events, and exact TTFT
byte offsets, alongside the existing provider parsing tests.

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

- `cargo test --workspace`: 308 passed; 28 existing opt-in tests ignored.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo fmt --all -- --check`: passed.
- UI `pnpm test`: 180 passed.
- UI `pnpm run check`: no errors or warnings.
- UI `pnpm run build`: static production build passed.
- Headless Chrome with synthetic auth responses: login icons, notification
  rendering, dark/light switching, dismiss buttons, and a 390 px viewport passed
  without runtime exceptions. No real backend/session data was used.
