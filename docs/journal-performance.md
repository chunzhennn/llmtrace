# Journal milestone verification

Measured on 2026-09-06 (Asia/Singapore) using synthetic traffic, local mock upstreams, and isolated PostgreSQL 16 databases. The capture journal was enabled with its default 1 GiB/100000-record limits and a temporary directory per process. Both archive backends therefore also used the local filesystem for journaling.

The host, workloads, and interpretation of timings follow the [earlier performance report](performance-testing.md): Ryzen 7 7735H, 16 logical CPUs, 26.6 GiB RAM, Linux, Rust 1.97.1, Node 24.18.1, and headless Chrome 151. Tests ran sequentially against release builds containing the production SPA. The harness uses four replay workers, a 1024-event intake queue, a 256 MiB event allocation budget, ten database connections, and 16 MiB HTTP capture limits. No real user data, paid provider, or model tokenizer was involved.

Raw results: [HTTP/streaming/browser/stall](performance/journal.json), [storage scale](performance/journal-storage.json), and [mixed traffic with rotation](performance/journal-sustained.json). The earlier raw reports are preserved as the pre-journal baseline. Storage/mixed results were captured before the final startup-only change to database-target fingerprinting; the end-to-end run and final functional checks include that change. These measurements are local observations, not deployment SLOs.

## Results

| Measurement | PostgreSQL archive | Filesystem archive |
| --- | ---: | ---: |
| Mixed 200 requests/second, p95 response latency | 0.449 ms | 0.412 ms |
| Mixed workload p99 | 4.816 ms | 4.918 ms |
| Mixed workload captures persisted before retention | 12000 / 12000 | 12000 / 12000 |
| Mixed workload sampled peak RSS | 345.5 MiB | 307.9 MiB |
| Mixed workload post-response drain | 0.052 s | 0.052 s |
| SSE first-body-byte p95, concurrency 64 | 6.555 ms | 5.730 ms |
| 32 × 8 MiB upload burst, concurrency 16, captures persisted | 32 / 32 | 32 / 32 |
| Stalled-database 64 × 8 MiB burst, captures dropped by memory admission | 8 / 64 | 8 / 64 |
| Stalled-database sampled peak RSS | 823.6 MiB | 688.9 MiB |
| Post-stall drain after releasing the database lock | 1.039 s | 0.516 s |

Every proxied HTTP request succeeded in these workloads. The stalled-database case preserved 56 of 64 captures on each backend; eight were rejected by the memory budget before journaling. All enqueued captures eventually persisted, and memory reservations returned to zero. At the end of the upload burst, the journal reported approximately 344 MiB/43 pending records on PostgreSQL and 336 MiB/42 on filesystem; remaining accepted captures were still entering the journal. A pending-record snapshot includes the active writer reservation, so it is not itself a durable-acknowledgement count. No journal capacity, read, write, or acknowledgement failure occurred in the performance runs.

Compared with the previous run, stalled-database drops decreased from 45/64 to 8/64 on each backend, while sampled RSS increased from 726.2 to 823.6 MiB and 637.3 to 688.9 MiB. Mixed-load RSS increased from 228.2 to 345.5 MiB and 215.4 to 307.9 MiB; p95 latency changed from 0.436 to 0.449 ms and 0.391 to 0.412 ms. These are successive local runs, not a controlled attribution of every change to the journal. The journal adds disk writes, raw-body reads, checksums, and allocation churn. It improves recovery and decouples intake from database stalls but does not remove capacity limits.

Each mixed-load run lasted 60 seconds, submitted 12000 requests at 200/second, and included an 8 MiB upload every hundredth request. Rotation checked a 32 MiB compressed-archive cap every second. Both runs ended with zero pending journal records/bytes, zero intake/replay memory reservations, no journal or retention failures, 599 retained requests, rollups matching retained requests, retained payload size below the cap, and no pending archive deletions. RSS rose across these short runs; they do not establish a long-term memory plateau.

The 50001-trace storage fixture measured request-list p95 2.49 ms, large-session detail 18.53 ms, and stats 1.04 ms across five reads each. Rotation removed 5101 traces in 1.44 seconds while a separate task inserted 100 traces sequentially with p95 insertion time 2.05 ms. These storage calls exercise indexed database/archive operations directly; they are not journal throughput measurements.

The browser authenticated and opened all main pages, including the populated capture-journal panel. There were no JavaScript/network errors or warnings. Request overview avoided eager body reads; Request/Raw tabs shared one fetch; previews stayed at or below 128 Ki characters; Copy retained the complete 8390335-character object using a clipboard stub. Raw-tab readiness was 114 ms, including the harness's fixed 100 ms settling delay. The first browser attempt failed because an assertion used rendered `innerText` against labels transformed to uppercase by CSS; it passed after the assertion used `textContent`. No product behavior was bypassed.

WebSockets completed 320 exact echo round trips per backend. A plugin intentionally exhausting its 100 ms execution limit did not stall forwarding; its 32-request burst had p95 response latency 2.03 ms and all captures drained.

## Functional and release checks

- 363 functional Rust tests passed, including six journal database integration tests, real process-kill/replay checks, GET/HEAD and cancelled-SSE completeness, configuration precedence, and corrupted-record/capacity handling.
- All three opt-in release performance tests passed. The storage and sustained tests passed in the first run; the end-to-end test passed on rerun after fixing its browser selector.
- 41 frontend tests passed; Svelte diagnostics reported zero errors/warnings; the production SPA and release binary built successfully.
- Rust formatting, Clippy with warnings denied, JavaScript syntax, and diff whitespace checks passed.
- The migration-only command applied embedded migrations successfully to local PostgreSQL, including `006_trace_journal.sql`.
- Compose configuration and CI YAML parsed successfully. The new workflow has not executed on GitHub, and a complete container-image build was not repeated for this milestone.

## Capacity and durability limits

Cold bursts still have variable tail latency: the first concurrency-64 1 KiB phase measured 76.77 ms proxy p95 versus 45.06 ms direct; the later filesystem phase measured 4.36 versus 2.26 ms. Phase ordering and connection setup confound backend comparisons. This milestone does not resolve that cold-tail behavior.

The 256 MiB budget bounds estimated captured-event allocations, not total process RSS. Live captures, parsing/compression/plugin scratch, and allocator retention need additional memory. A 256 MiB pod limit is unsuitable for the tested large-payload concurrency. Forwarding remains best effort with respect to auditing: bursts, journal exhaustion, or write failures can reject captures, and in-flight/pre-sync captures can be lost on process death. Full journals preserve existing evidence and reject new captures; the separate archive cap evicts already-persisted history.

Crash tests kill an isolated helper at synchronized-write and post-commit boundaries. They do not simulate power failure, real disk exhaustion, filesystem/device failure, or multi-replica failover. The short table-lock workload is not a prolonged network partition; a separate functional test injects database write errors and verifies automatic retries while new captures continue journaling. Multi-hour tests under actual pod limits, disks, TLS/ingress, provider latencies, plugins, and expected traffic remain necessary for production capacity acceptance.

See the [durable capture guide](durable-capture.md) for acknowledgement semantics, unredacted credential handling, persistent volumes, quarantine operations, and startup/readiness limits. Reproduce with the commands in the [performance report](performance-testing.md); use separate report paths to preserve historical samples. CI exposes these same release tests through its manual `performance` input.
