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
