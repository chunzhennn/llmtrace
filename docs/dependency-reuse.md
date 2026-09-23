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
