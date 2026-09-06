# OpenRouter live verification

On 2026-09-06 (Asia/Singapore), 17 live HTTP cases passed through a local release build of llmtrace using isolated PostgreSQL databases and temporary filesystem archives/journals. Sixteen model requests succeeded; the intentionally invalid model returned HTTP 400 and was recorded correctly. The sum of provider-reported generation charges was **$0.000972279**, about one tenth of one cent. Authentication/catalog lookups did not generate model tokens.

The tests used `openai/gpt-4.1-nano` and `google/gemini-2.5-flash-lite`. At test time, OpenRouter's [model catalog](https://openrouter.ai/api/v1/models) listed both at $0.10 per million input tokens and $0.40 per million output tokens. Outputs were capped at 32 or 96 tokens, and no paid search or other provider-side tools were enabled. See the [sanitized results](openrouter-live-results.json) for individual cases, usage, timings, assertions, and reported charges.

## Verified behavior

| Area | Live checks |
| --- | --- |
| Chat Completions | JSON and SSE responses; two models; usage extraction and conversation grouping. |
| Responses | JSON and SSE responses, including function calls and streamed arguments. |
| Messages | Anthropic-compatible JSON/SSE requests through OpenRouter with Gemini, including tool-use blocks and streamed arguments. This does not test a direct Anthropic account. |
| Tool calls | Name and parsed arguments match `add(2, 3)` in all three formats, streamed and non-streamed. A follow-up request uses the real returned Chat Completions tool-call ID and a local result of `5`; the model completes the round trip and the follow-up trace persists. |
| Archived content | Byte-for-byte comparisons of both request and received response bodies for the 16 primary cases. The separate tool-result follow-up is checked for persistence and successful completion. |
| Larger request | A 63234-byte synthetic request with 9020 reported input tokens. The exact archive remains complete; the bounded session-message preview correctly carries `session_messages_truncated`. This is not a million-token live test. |
| Usage and estimates | Stored input/output counts match the provider's returned fields; successful requests receive configured price estimates. |
| Timing | TTFB is recorded; SSE TTFT identifies the first generated output. Non-streaming TTFT remains null because token-generation time is not observable from a complete JSON response. |
| Error capture | Invalid model returns/stores HTTP 400, with no invented usage or price. |
| Identity plugin | A WASM fixture reads the captured authorization header, calls the allowlisted current-key endpoint, and attaches its real account-owner ID to the session. The fixture contains no embedded credential. |
| Admin access and redaction | Unauthenticated stats return 401; local admin login enables stats. Persisted request details do not contain the supplied key. |

The identity endpoint exposes `creator_user_id`, an OpenRouter account owner, rather than the individual employee behind a shared key. The fixture labels this as an account owner, and the report omits the identifier. Enterprise employee attribution still requires individual credentials or the enterprise gateway's identity endpoint. See OpenRouter's [current-key reference](https://openrouter.ai/docs/api/api-reference/api-keys/get-current-api-key).

The configured cost is an **estimate**, rounded to micro-USD. Provider-reported charges differed slightly from catalog-based estimates in this run. Discounts and other billing adjustments are not automatically applied to llmtrace's structured estimate fields; the provider's returned `usage.cost` remains available in the archived response. OpenRouter documents that field in [usage accounting](https://openrouter.ai/docs/cookbook/administration/usage-accounting).

## Routing fix discovered by the test

The original default-upstream resolution replaced its entire path with the incoming path. With `default_upstream = "https://openrouter.ai/api"`, a client request to `/v1/chat/completions` therefore resolved to `https://openrouter.ai/v1/chat/completions`. The configured `/api` allowlist rejected that URL before forwarding.

Default-upstream resolution now preserves the deployment prefix, producing `/api/v1/chat/completions`. It avoids duplicating a prefix already present in the incoming path and retains query strings and encoded path components. Regression cases cover origin-only URLs, trailing slashes, already-prefixed paths, segment boundaries, and encoded characters. Existing configurations containing a non-root default-upstream path now use that path; check that it is the intended deployment prefix.

For clients that send `/v1/...`, configure OpenRouter as:

```toml
[proxy]
default_upstream = "https://openrouter.ai/api"
allow_upstreams = ["https://openrouter.ai/api"]
path_prefixes = ["/v1"]
```

The `/api` path is the upstream deployment prefix; `/v1` comes from the client request. Explicit upstream override headers continue to support a full destination URL under the existing allowlist rules. No new configuration field or database migration is required for this fix.

## Reproduce

The opt-in test is [storage/openrouter.rs](../crates/llmtrace/src/storage/openrouter.rs). It requires local PostgreSQL with permission to create test databases, a release binary containing the current code, network access to OpenRouter, and a private file containing the API key. The test removes its temporary proxy/journal/archive directory, but callers own the credential file and should remove it when finished. The credential used for this recorded run was removed after testing; it is absent from source and reports.

```sh
cargo build --release --workspace --locked
export DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/llmtrace
export LLMTRACE_RUN_OPENROUTER=1
export OPENROUTER_API_KEY_FILE=/absolute/path/to/private-key-file
export LLMTRACE_OPENROUTER_REPORT=/tmp/llmtrace-openrouter-report.json
cargo test --workspace --locked openrouter_live -- --ignored --nocapture --test-threads=1
```

The key file should have mode `0600`. `LLMTRACE_OPENROUTER_CASES` can select comma-separated names, for example `responses_tool_call,messages_tool_call_stream`, to avoid repeating paid cases. The harness checks current catalog rates against $0.11/M input and $0.41/M output ceilings and stops submitting the primary cases after two cents of reported charges. Fixed request sizes/counts and output limits keep expected usage well below that threshold; this is not a provider-enforced spending limit. There are no automatic inference retries or switches to more expensive models.

The recorded run executed the original 13 cases followed by four additional tool-format cases, using two isolated databases. It is functional compatibility evidence, not a load test or measurement of proxy-only overhead. Provider/network time dominates these latencies. Live WebSockets, cancellations, retry faults, and large-scale retention were not exercised against OpenRouter; see the [journal](journal-performance.md) and [earlier performance](performance-testing.md) reports for local coverage.

After the routing fix, all **365 local functional Rust tests**, formatting, Clippy with warnings denied, and the release build passed. CI explicitly excludes the paid test, which also requires the opt-in environment flag. Frontend code and persisted schema did not change in this pass.
