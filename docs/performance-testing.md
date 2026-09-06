# Local performance verification

This report records the **pre-journal** performance pass. The current default pipeline also journals captures to disk; see [journal performance results](journal-performance.md) for the subsequent measurements. Historical numbers below have been preserved. The reproduction commands run the current checkout.

Tested on 2026-09-05/06 using synthetic traffic and isolated PostgreSQL databases. This pass found and fixed avoidable streaming delay, large-buffer copies, an audit queue bounded only by event count, database locks held during compression, and expensive rendering of large raw JSON.

## Changes driven by testing

- Enable `TCP_NODELAY` on accepted proxy sockets. Small SSE chunks previously incurred roughly 40 ms of extra delay on local connections.
- Move the captured upload buffer into the trace event when a response completes. The live path no longer clones a multi-megabyte upload. Late upload chunks still enforce the request limit without reallocating an abandoned capture.
- Add `storage.trace_queue_max_bytes`, default **268435456 bytes (256 MiB)**, also configurable as `LLMTRACE_TRACE_QUEUE_MAX_BYTES`. Allocated body capacity and estimated event metadata count against one shared budget through queueing and active processing. Reservations release on completion and failed enqueue. Saturation drops audit events and increments metrics; it does not block forwarding. Drop warnings are sampled to avoid a log storm.
- Finish archive framing, hashing, and compression on blocking workers before acquiring database connections or session/rollup locks. Original body buffers are released during preparation.
- Bound the raw JSON preview before serialization and DOM rendering, including unusually large field names, wide arrays, and deep structures. Explicit Copy still serializes the complete captured object. Add the missing favicon detected by browser navigation.
- Expose audit memory usage, budget, and dropped-event counters through Prometheus, the stats API, and the admin UI.

These changes require no additional migration beyond the usage and rotation migrations from the enterprise audit.

## Environment and method

The machine reports an AMD Ryzen 7 7735H, 16 logical CPUs, and 26.6 GiB RAM. Tests used Linux, Rust 1.97.1, PostgreSQL 16 in local Docker, Node 24.18.1, and headless Chrome 151. The frontend was built before the Rust release binary so the real SPA was embedded.

Tool versions and the measured binary SHA-256 hashes are in [performance/environment.json](performance/environment.json).

The application runs as a separate release process. The mock upstream and generator run outside that process; reported RSS therefore excludes them and PostgreSQL. Linux `/proc` RSS is sampled every 10 ms during load. Both mock and proxy accepted sockets use `TCP_NODELAY` after the fix. Tests run sequentially, without a simultaneous build or second benchmark.

The main harness uses four trace workers, a 1,024-event queue, a 256 MiB event memory budget, a ten-connection database pool, and 16 MiB HTTP capture limits. Payloads contain deterministic pseudo-random alphanumeric text, avoiding misleading compression ratios from repeated characters. “8 MiB” describes text content; the JSON envelope adds 58 bytes. The smaller request is 1,082 bytes including its envelope. No model tokenizer, real employee data, identity service, or paid provider was involved.

## Coverage and assertions

| Area | Workload and checks |
| --- | --- |
| Functional backend | 349 passing tests, including eight isolated-database integration tests. Parsers, usage/costs, redaction, config/environment precedence, auth, proxy streaming/limits, WASM HTTP constraints, archive round trips/fallback, retention, rollups, and structured queries. |
| Functional frontend | 41 passing tests; Svelte diagnostics with zero errors/warnings; production build. |
| HTTP load | Direct-upstream and proxy controls; 1 KiB requests at 1, 16, and 64 concurrent clients; 200 requests/second; 8 MiB uploads at 4 and 16 concurrent clients; 8 MiB responses. Successful status and response length are asserted. |
| Streaming | 128 requests at concurrency 16 and 256 at concurrency 64, with twelve delayed SSE events. Client-observed first-body-byte and completion latency recorded. Separate functional integration checks exact forwarded SSE bytes and persisted usage/timing. |
| WebSockets | Sixteen simultaneous connections, twenty exact echo round trips each, on both archive backends. |
| Database stall | Hold an exclusive trace-table lock and submit sixty-four 8 MiB uploads at concurrency eight. Assert forwarding succeeds, memory reservations remain within budget, overload is counted, and accepted traces drain after releasing the lock. |
| Slow plugin | A WASM hook loops until its 100 ms deadline. Thirty-two requests at concurrency sixteen continue forwarding and all accepted traces persist. |
| Storage scale | 50,001 traces/segments and approximately 100,000 archive records in one large session. Time request list, session detail, and stats reads; rotate while another task performs 100 inserts. Assert size cap and rollup count match. |
| Mixed traffic with rotation | Sixty seconds per backend at 200 requests/second: 12,000 requests, every hundredth carrying 8 MiB. Rotate at 32 MiB with one-second checks. Assert every trace persisted before retention, no pipeline or retention failure, zero remaining memory reservations, oldest data evicted, rollups match retained traces, cap reached, and pending file deletions cleared. |
| Real browser | Authenticate and navigate overview, requests, sessions, analytics, and system pages. Request overview must not fetch bodies. Request/Raw tabs share one lazy body fetch, render at most 128 Ki characters, and Copy retains the full 8 MiB object. Capture JS/network failures and rendering metrics. Clipboard is stubbed inside the temporary browser tab. |

The three performance tests are opt-in and passed in release mode. Rust formatting and Clippy with warnings denied also passed. They assert correctness and bounded resource accounting, rather than hard-coding machine-specific latency thresholds.

## Measurements

Raw samples are saved in [performance/baseline.json](performance/baseline.json), [performance/after.json](performance/after.json), [performance/storage.json](performance/storage.json), and [performance/sustained.json](performance/sustained.json). Baseline means the working tree immediately before this performance pass, including the earlier enterprise audit and size-rotation fixes; it is not the repository's original implementation. Comparative cases use the same local upstream and workload shapes. The steady-traffic and slow-plugin cases, along with new memory-budget assertions, were added after baseline capture.

| Measurement | PostgreSQL archive | Filesystem archive |
| --- | ---: | ---: |
| SSE first-body-byte p95, concurrency 64, before | 47.63 ms | 47.80 ms |
| SSE first-body-byte p95, concurrency 64, after | 8.22 ms | 6.66 ms |
| Mixed 200 requests/second with rotation, p95 | 0.436 ms | 0.391 ms |
| Mixed workload p99 | 4.61 ms | 4.54 ms |
| Mixed workload sampled peak process RSS | 228.2 MiB | 215.4 MiB |
| Mixed workload audit events persisted | 12,000 / 12,000 | 12,000 / 12,000 |
| Stalled database sampled peak RSS, before | 912.2 MiB | 814.6 MiB |
| Stalled database sampled peak RSS, after | 726.2 MiB | 637.3 MiB |
| Stalled database audit events dropped, after | 45 / 64 | 45 / 64 |

All requests succeeded during the stalled-database case. The memory budget remained below 256 MiB and returned to zero after draining. This is a deliberate availability-versus-audit-completeness tradeoff: dropped records cannot be recovered from this in-memory pipeline.

The 32-request, 8 MiB upload burst at concurrency sixteen also exhausted the default memory budget on PostgreSQL: ten traces were dropped in the recorded run. Filesystem persisted all thirty-two in its run. Lower response latency in an overload case must not be interpreted as improved capacity while preserving all audits.

On the 50,001-trace fixture, request-list p95 was 3.12 ms, large-session detail 18.89 ms, and stats 1.33 ms. These percentiles summarize only five reads each and describe warm local database calls, not a production tail-latency distribution. Rotation deleted 5,101 requests in 1.40 seconds while the concurrent insertion task had 2.35 ms p95 insert time; retained size ended below the configured cap and rollups matched the remaining traces. The 100 inserts are sequential in one task running concurrently with rotation, not 100 simultaneous writers.

For the 8 MiB raw JSON browser view, rendered text fell from 8,390,374 to approximately 131,000 characters and harness readiness from 677 ms to approximately 117 ms. Full-object copying and one-fetch lazy loading passed. Browser readiness includes a fixed 100 ms settling delay for tabs (150 ms for navigation); it is not a Core Web Vitals measurement. There were no final browser JS or network errors.

## Interpretation and remaining limits

- These are local release measurements on one host. The mock immediately replies or emits short scheduled chunks; TLS, enterprise ingress, provider connection setup, realistic generation duration, network loss, and real identity lookup latency are not represented. SSE first-byte measurements here are not model TTFT.
- Cold bursts still have variable tail latency. The final 1 KiB, concurrency-64 burst measured 86.83 ms p95 on the PostgreSQL-backed proxy versus 34.13 ms direct; the later filesystem phase measured 5.10 ms versus 2.62 ms direct. Both persisted all 1,024 traces. The exact cause of the cold-phase spikes has not been isolated, and phase order/connection reuse confounds a backend comparison. Profile connection setup and scheduling under the deployment's concurrency before accepting a tight tail-latency target. Do not subtract individual sub-millisecond samples to infer negative overhead or treat short-burst requests/second as sustainable audited capacity.
- The 256 MiB budget covers estimated queued/active event allocations, **not total process memory**. Live concurrent captures, worker parsing/compression/plugin scratch, metadata, allocator retention, and other application work require additional memory. Large-payload stress still exceeded 600 MiB RSS. A pod limit of 256 MiB is not justified by this setting.
- The minute-long mixed runs demonstrate no lost traces or accumulating queue at their end. RSS grew during the runs; they do not establish a stable long-term plateau or exclude a slow leak. Multi-hour tests under deployment CPU/memory limits remain necessary for capacity planning.
- Rotation limits indexed compressed archives asynchronously. Database metadata/indexes/WAL, operational logs, and orphaned unindexed files are outside the cap. The fixture tests 50,001 requests, not millions of requests or multi-terabyte storage. No disk-full, abrupt-kill, device failure, or multi-replica fault injection was performed in this pass.
- Long-lived WebSockets, per-generation WebSocket accounting, concurrent admin investigations, broad expensive structured filters, and large exports need deployment-specific load coverage. See the [enterprise audit](enterprise-audit.md) for functional boundaries.

For a deployment acceptance run, use its pod CPU/memory limits, ingress/TLS path, PostgreSQL/storage topology, real plugins, payload distribution, and expected concurrency. Watch `llmtrace_trace_pipeline_dropped_memory_total`, `llmtrace_trace_pipeline_dropped_full_total`, persistence/build failures, queue depth, event memory, process RSS, and retention failures together. Audit completeness requires all drop/failure counters to remain zero at the accepted workload. Adjusting the budget moves the memory-versus-dropping threshold; it does not add storage throughput or durability.

## Reproduce

The SQLx tests require a PostgreSQL role that can create isolated test databases. They migrate and delete synthetic fixtures in those databases; the database named in `DATABASE_URL` is used for test administration. Mock servers bind only to loopback. Browser runs require Node with built-in `WebSocket` and Chrome/Chromium; `CHROME_BIN` overrides `/usr/bin/google-chrome`.

```sh
docker compose up -d postgres
export DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/llmtrace

corepack pnpm --dir crates/llmtrace/ui install --frozen-lockfile
corepack pnpm --dir crates/llmtrace/ui check
corepack pnpm --dir crates/llmtrace/ui test
corepack pnpm --dir crates/llmtrace/ui build

cargo fmt --all -- --check
cargo test --workspace --locked -- --include-ignored --skip performance_ --skip journal_process_helper --skip openrouter_live
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --release --workspace --locked -j 4

LLMTRACE_PERF_BROWSER=1 \
LLMTRACE_PERF_REPORT=/tmp/llmtrace-performance.json \
LLMTRACE_PERF_STORAGE_REPORT=/tmp/llmtrace-storage.json \
LLMTRACE_PERF_SOAK_REPORT=/tmp/llmtrace-soak.json \
cargo test --release --workspace --locked performance_ -- \
  --ignored --nocapture --test-threads=1
```

Omit `LLMTRACE_PERF_BROWSER` when Chrome is unavailable; that skips real-browser coverage. `LLMTRACE_PERF_BINARY` can point at a separately saved release executable for comparisons. The harness removes inherited `LLMTRACE_*`/`DATABASE_URL` overrides from the child application and supplies its generated isolated config. Successful runs remove temporary application files; failed fixtures/logs remain under `/tmp/llmtrace-perf-*` for diagnosis. The harness and browser runner are in [storage/performance.rs](../crates/llmtrace/src/storage/performance.rs) and [browser-performance.mjs](../scripts/browser-performance.mjs).
