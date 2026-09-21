# Full session export

llmtrace records and exports captured traffic. Run scheduling, conversation review,
summarization, and skill creation in your own workflow. The application does not
call a model or run an analysis job when you export.

Use **Export full session** on the session detail page, or call:

```text
GET /api/sessions/{session-id}/export.jsonl
```

This authenticated endpoint exports **all retained requests in one session**, in
ascending `(started_at, id)` order. It has no message or request page limit. The
existing `/messages/export.jsonl` and `/requests/export.jsonl` endpoints continue
to export bounded preview/summary pages.

The attachment uses `application/x-ndjson`, a session-specific filename, and
`Cache-Control: no-store`. Requests, message previews, archive references and
Postgres blobs are read from one read-only, repeatable-read snapshot. Captures
persisted after that snapshot appear in the next export. Each database statement
retains the 5 second read timeout. Export generation streams one request at a time,
with at most two exports per process and a 15 minute deadline. Capacity exhaustion
returns `429` with `Retry-After: 5`; a missing session returns `404`.

## Version 1 records

Each line is a JSON object. A successful file has these records in order:

| `type` | Contents |
| --- | --- |
| `session` | `schema_version: 1`, `exported_at`, session metadata in `session`, and the expected `request_count`. |
| `request` | Request metadata in `request`, original `request_body` and `response_body`, and supplementary `message_previews`. One record per retained request. |
| `end` | `session_id`, `export_complete: true`, actual `request_count`, `message_preview_count`, `truncated_body_count`, `unavailable_body_count`, and `captured_bodies_complete`. |

`request` has the metadata fields of `GET /api/requests/{id}`, including IDs,
timestamps, model, status/error, usage, tools, tags, stored headers and plugin
metadata. Its body strings, `bodies_included`, `request_body_status`, and
`response_body_status` fields are replaced by the two body objects at the record's
top level:

```json
{
  "status": "available",
  "encoding": "utf8",
  "data": "{\"messages\":[{\"role\":\"user\",\"content\":\"hello\"}]}",
  "captured_bytes": 48,
  "truncated": false
}
```

- `encoding` is `utf8` when the captured bytes are valid UTF-8, otherwise `base64`
  using the standard alphabet with padding. Decoding `data` recovers the exact
  archived bytes, including embedded newlines. `captured_bytes` counts those bytes;
  request metadata's `request_body_bytes`/`response_body_bytes` count observed traffic.
- `status` is `available`, `missing` (no stored payload despite observed bytes), or
  `unreadable` (archive I/O, checksum, or decoding failure). Unavailable bodies have
  `encoding`, `data`, and `captured_bytes` set to `null`. A captured empty body is
  `available` with `data: ""` and `captured_bytes: 0`.
- `truncated` preserves the capture completeness flag. Archive failures do not
  discard the other body or other requests; they increase `unavailable_body_count`.
  Historical zstd bodies stored inline in Postgres are also exported.

Use the original bodies as conversation evidence. JSON payloads retain system and
developer instructions, all messages, reasoning/content blocks, tool definitions,
call IDs, arguments, results, and multimodal references that were captured. SSE
responses retain their original events rather than only the generated text.
WebSocket captures retain the representation stored by the proxy. No external
image/file URLs are fetched. Provider-side conversation history that never passed
through the proxy is not available.

`message_previews` contains all persisted previews for the request, with the same
shape as session detail messages. Previews can be shortened by ingestion limits
and can omit non-text content; consult request tags such as
`session_messages_truncated`. Raw body export is independent of those preview
limits. Requests often resend earlier conversation history: the export preserves
each request snapshot without deduplicating, merging branches, or guessing order
between concurrent requests. IDs and timestamps let your workflow decide how to
reconstruct the conversation.

## Download and validate

Use the existing login API to obtain a cookie jar, or supply a valid session cookie
from your OAuth login. For local login, `login.json` contains your `username` and
`password`:

```bash
umask 077
curl --fail-with-body -c cookies.txt \
  -H 'Content-Type: application/json' --data-binary @login.json \
  'http://127.0.0.1:3000/api/auth/login'
```

The following example leaves an existing successful export intact if a new download
fails. It checks the terminal record before accepting the file:

```bash
set -eu
umask 077
LLMTRACE_URL='http://127.0.0.1:3000'
SESSION_ID='replace-with-session-uuid'
curl --fail --show-error --cookie cookies.txt \
  "$LLMTRACE_URL/api/sessions/$SESSION_ID/export.jsonl" \
  --output "$SESSION_ID.jsonl.part"

python3 - "$SESSION_ID.jsonl.part" "$SESSION_ID" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source:
    header = json.loads(next(source))
    if (header.get("type") != "session" or header.get("schema_version") != 1
            or header["session"]["id"] != sys.argv[2]):
        raise SystemExit("Unexpected export header")
    count = 0
    end = None
    for line in source:
        record = json.loads(line)
        if end is not None:
            raise SystemExit("Unexpected data after end record")
        if record.get("type") == "request":
            if record["request"]["session_id"] != sys.argv[2]:
                raise SystemExit("Unexpected request session")
            count += 1
        elif record.get("type") == "end":
            end = record
        else:
            raise SystemExit("Unexpected record type")
    if (end is None or end.get("export_complete") is not True
            or end.get("session_id") != sys.argv[2]
            or count != end.get("request_count") or count != header["request_count"]):
        raise SystemExit("Incomplete export; retry without replacing the previous file")
    if not end["captured_bodies_complete"]:
        print("Export finished with truncated or unavailable bodies:",
              end["truncated_body_count"], end["unavailable_body_count"], file=sys.stderr)
PY
mv "$SESSION_ID.jsonl.part" "$SESSION_ID.jsonl"
```

An HTTP `200` is insufficient: a disconnect, database error, or export deadline can
interrupt the stream. Require the `end` record and matching request counts.
`export_complete` means every request in the snapshot was exported;
`captured_bodies_complete` means all exported bodies were available and untruncated.
Neither proves that the proxy observed an entire conversation. Retention, queue
drops, pending journal replay, capture limits, and provider-side history still
define the available evidence. Filesystem archives removed during export are
reported as unavailable. Re-export after pending captures persist when needed.

Stored headers keep their existing redaction; bodies and plugin metadata retain
their stored content. Export files therefore carry the same sensitive conversation
data as the authenticated request detail API.

## Periodic workflow integration

Use the existing structured query API to select sessions active during a time
window. For example, send this to `POST /api/query` with the same authentication:

```json
{
  "dataset": "sessions",
  "fields": ["id", "first_seen", "last_seen", "user_id", "user_name"],
  "filters": [
    {"field": "last_seen", "op": "gte", "value": "2026-09-15T00:00:00Z"},
    {"field": "last_seen", "op": "lt", "value": "2026-09-16T00:00:00Z"}
  ],
  "order_by": [{"field": "id", "direction": "asc"}],
  "limit": 500
}
```

Download each returned session ID through the full export endpoint. To enumerate
more than 500 sessions, keep the same time bounds and add an `id > last_id` filter
using `{"field":"id","op":"gt","value":"<last ID from previous page>"}`;
repeat until `rows` is empty. Omit the time filters to reconcile all retained
sessions. Use session IDs as stable file keys and request IDs to deduplicate evidence
across repeated exports.

`last_seen` is traffic event time, not an ingestion watermark. Use overlapping
windows and re-export active sessions; delayed journal replay can add requests
with older event times. Periodic full reconciliation covers data delayed beyond
your lookback. The list queries are independent snapshots, so newly committed
sessions may only appear on a subsequent scan. Scheduling and checkpoint storage
belong to the consuming workflow.
