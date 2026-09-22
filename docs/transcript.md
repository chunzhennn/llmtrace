# Full transcript and repeated context

The dedicated `/ui/sessions/{id}/transcript` page reads the authenticated full
session JSONL export. It does not use `session_messages` as its content source.
The export reads one request at a time from a read-only, repeatable-read snapshot,
in `(started_at, id)` order, through the existing shared archive body reader.
The preview's 16 KiB text and 128-message limits do not apply to this page.

A Web Worker downloads, decodes, parses and compares records off the UI thread.
The reader joins network fragments once per JSONL record, and waits for a UI
acknowledgement before consuming the next record. It does not buffer the whole
export. It requires a matching header, request count and final end record, followed
by EOF. A disconnect leaves the content already shown with an error; retry reads a
fresh snapshot and replaces that view, rather than appending duplicate requests.
Leaving the page terminates the worker and cancels its request.

Chat Completions, Responses and Anthropic Messages are supported in JSON and SSE
form, including tools, reasoning, refusals, alternatives and structured content.
Non-text message blocks and unknown fields are shown as JSON; references and
binary base64 content are retained without fetching external resources. Unknown
or malformed events are shown as captured content. Missing, unreadable, truncated
bodies and unterminated streams are identified beside the affected request. A
preview truncation tag alone does not label a complete archive as incomplete.

## Context matching

Context reuse compares a request's input against a prefix trie of previously
observed conversation paths. Identity includes role, exact content, tool/call IDs,
arguments, names and non-text/unknown message fields. Object key order is
normalized. Plain text and a single equivalent text block match; empty optional
chat fields such as `refusal: null` match their absence. Content whitespace and
array order are never normalized. Request settings such as temperature remain
available on the source Request page and full export.

Only a contiguous, exact input prefix can be folded. After the first difference,
all remaining input messages are displayed, even if some individual texts have
appeared before. Every newly generated output is displayed, including identical
answers from retries. Alternative choices extend separate paths. Incomplete
captures are not used as evidence to suppress future content. Sliding windows,
changed message metadata, and provider-managed history that cannot be matched
unambiguously stay visible. This intentionally prefers some repetition over
incorrectly hiding a turn. Requests with `previous_response_id`, `conversation`,
or input item references keep every input visible and do not establish reusable
prefixes, because their provider-managed history is not present in the capture.
The parser reports context type and input/output completeness explicitly;
display notices do not control matching.

Each folded context keeps a reference to its terminal trie node. Expanding it walks
parent references to the original message content and shows the context with the
current request's source link. Repeated context is not copied into another large
DOM subtree until expanded. Matching costs are proportional to the input being
read, rather than scanning all prior conversations for every message. Retained
state grows with distinct conversation paths and displayed messages, not with a
copy of every repeated input snapshot. Each message renders as one continuous text
flow, preserving original line breaks both on screen and when copied. Off-screen
message layout is deferred with `content-visibility: auto`. Memory still includes
one raw request record and the unique content of
the session, so it is not constant for arbitrarily large sessions. Existing export
concurrency and timeout limits continue to apply.

## Provided tools

Each request with tool declarations has a **Tools available** panel in the
transcript. It lists all offered tools, including tools that were never called,
with their supplied names and descriptions. **Parameters and full definition**
expands the complete original definition, including JSON Schema, strict mode and
provider-specific options. These definitions also appear in the Request page's
separate **Declared tools** tab. Request tabs can be linked directly with
`?tab=declared-tools`, `?tab=request` or `?tab=tools`; opening the Overview does
not download bodies.

Chat Completions nested function definitions, Responses flat definitions,
Anthropic `input_schema`, and legacy `functions` are supported. Built-in or
unrecognized tools retain their original definition; missing descriptions are
identified without inventing a description.

The transcript worker interns complete declaration sets. An unchanged set is
transferred once and referenced by later requests, whose panels start collapsed.
Changed or restored definitions start expanded, even if tool names are unchanged.
Full schemas enter the DOM only when expanded. Tool definitions are independent
of message-context folding: changing a schema cannot hide that change behind a
reused conversation prefix. Requests without declarations do not inherit an
older request's tools, and unavailable bodies cannot supply definitions.

## Request tool calls

The Request page's **Tool calls** tab separates calls carried in the input history
from new calls generated by the response. Input calls also appear in the **Request**
tab, with arguments and matching tool results. A response that only summarizes a
previous result can therefore have input calls and zero new calls. Stored request
metrics continue to count newly generated response calls.

Chat Completions, Responses and Anthropic Messages input calls and results are
read from the captured JSON. A single pass matches each result to the most recent
preceding call with exactly the same call ID; only legacy function messages match
by name. Unmatched results remain visible, including output-only inputs whose
history is managed by the provider. Repeated calls are retained. Arguments and
results are not shortened; contents longer than 8 KiB start collapsed and enter
the DOM when expanded. Missing or malformed bodies display an availability notice.

## Preview ellipses

The Session detail preview continues to use bounded stored messages. Migration
007 adds nullable `session_messages.content_truncated`, populated per message by
the existing background ingestion step. A shortened preview receives a visual
ellipsis and a **Content shortened** badge; its stored content is unchanged.
Role/count limits or another shortened message in the request do not add a badge
or ellipsis to a complete reply. Request-level capture warnings remain in the
request table and details.

Historical rows have `null` until verified against their original capture. The
migration does not guess from message length or decompress all archives. In
particular, an exact 16 KiB complete message must not be mislabeled just because
another message in its request was shortened. This flag is available on session
messages, export previews and the allowlisted `messages` query dataset.
