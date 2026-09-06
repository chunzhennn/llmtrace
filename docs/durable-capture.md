# Durable capture and replay

The capture journal is enabled by default. It separates accepting audit captures from parsing, plugin execution, compression, and database persistence. A running proxy can journal completed requests while database writes are unavailable, then replay those captures after recovery or restart.

## Durability boundary

```text
stream upstream traffic → bounded memory queue → journal file + directory sync
                                               ↓
                                      bounded replay workers
                                               ↓
                         archive + trace + messages + rollups + receipt commit
                                               ↓
                                journal deletion + directory sync
                                               ↓
                                      receipt cleanup
```

Forwarding never waits for journal I/O or database work. `TraceRecorder::record` still uses nonblocking admission and drops captures when its event-count or estimated-memory budget is exhausted. The background writer stores each completed capture as a versioned, checksummed file, synchronizes it, renames it into place, and synchronizes its directory before making it available for replay. Filesystem archives are also synchronized before their database references commit.

**A successful HTTP response is not a durable audit acknowledgement.** In-flight HTTP/WebSocket captures and captures waiting in memory or undergoing their first disk write can be lost on process death. The `written` counter advances only after journal synchronization; `not_yet_durable` tracks accepted captures that have not reached that boundary or a reported admission failure. `pending_records` includes a writer reservation during an append, so that gauge alone is not proof of durability. Once synchronized, a capture remains pending until database commit and durable journal deletion succeed. This relies on the filesystem and device honoring synchronization; the tests do not simulate power loss or certify storage hardware.

Each database transaction inserts an ingest receipt before updating the trace, messages, session, archive indexes, and rollups. Replaying a committed receipt skips those updates. Receipts outlive trace retention, so a crash between commit and journal deletion cannot resurrect a rotated trace. Receipt cleanup happens only after the journal no longer contains the record or its quarantine entry. Repeated filesystem writes after a transaction rollback reuse paths derived from the journal, trace, and compressed payload checksum.

## Configuration

File settings load first; environment variables override them. Restart to apply changes.

```toml
[storage.journal]
enabled = true
directory = "spool/journal"
max_bytes = 1073741824 # 1 GiB, uncompressed captures plus framing/metadata
max_records = 100000
retry_interval_secs = 2
```

| TOML field | Environment override |
| --- | --- |
| `enabled` | `LLMTRACE_JOURNAL_ENABLED` |
| `directory` | `LLMTRACE_JOURNAL_DIRECTORY` |
| `max_bytes` | `LLMTRACE_JOURNAL_MAX_BYTES` |
| `max_records` | `LLMTRACE_JOURNAL_MAX_RECORDS` |
| `retry_interval_secs` | `LLMTRACE_JOURNAL_RETRY_INTERVAL_SECS` |

Capacity counts pending and quarantined record files. The limits must be positive, up to 1 TiB and 1,000,000 records; retry intervals range from 1 to 3600 seconds. Reducing a limit below existing usage preserves files and rejects new captures until there is room. A capture larger than the entire byte limit is rejected. **A full journal preserves older pending captures and drops new captures**, reporting the failure rather than deleting evidence that has not reached the database.

Journal capacity is independent of `storage.rotate_size_bytes`, which evicts the oldest persisted traces and compressed archives. The journal cap excludes filesystem block/inode overhead, its manifest/lock, archives, database/WAL files, and application logs. Leave free-space headroom for all of them. The journal stores uncompressed raw bodies; size it using expected captured bytes and the outage duration to bridge, not compressed archive sizes.

`storage.trace_queue_max_bytes` covers estimated queued and replay-active events. If a recovered record requires more than that budget, it stays on disk and increments `blocked_records`; raise the budget after checking available process memory. Live captures, parsing/compression/plugin scratch, and allocator overhead remain outside this budget. Disabling the journal restores the previous in-memory pipeline and does not replay or delete an existing journal directory.

## Deployment and data handling

Use a persistent filesystem with working exclusive file locks, atomic rename, and file/directory synchronization. Give each running instance its own stable journal directory and preserve that directory across replacement. Sharing one journal directory between processes is rejected by an exclusive lock. The directory manifest binds receipts to that journal and database target; a different database target or a missing manifest alongside existing captures fails startup. A credential-only database URL change is allowed. Keep the entire directory, including its manifest, together during moves.

All instances sharing a database still need a shared, stable **archive** root for archive reads and retention. Journals are separate per instance. Compose now mounts a named `llmtrace-spool` volume at `/var/lib/llmtrace/spool`; the image prepares that location for UID/GID 10001. Existing deployments with archives in a container's writable layer must copy their existing spool to persistent storage before replacing the container; adding a volume does not migrate those files.

Journal records include captured bodies and the original credential-bearing headers supplied to enrichment plugins, before persistence redaction. On Unix, the journal directory is mode `0700` and new files are `0600`. These permissions are not encryption. Apply the same restricted access, encrypted-storage, and backup controls as for provider credentials and employee conversations. Archive bodies remain unredacted as documented elsewhere.

Coordinate backups/restores of PostgreSQL, filesystem archives, and each journal. Restoring an old journal against a newer database after its receipts were cleaned is not a supported replay/import procedure. Keep the database target and journal manifest stable; there is no automatic reassignment or journal format upgrade tool. Replay uses the currently configured parser, pricing, and plugins, so enrichment can change across a configuration or software update and identity lookups can run again or fail if the original credential has expired.

Startup still connects to PostgreSQL and runs migrations before serving. `/readyz` still requires a successful database query. The journal bridges database write interruptions in an already-running instance; it does not provide database-independent startup, and a scheduler that removes unready instances may still interrupt proxy availability. After live connections finish, shutdown allows ten seconds for pipeline draining; synchronized pending records remain for restart after that deadline. Already-running blocking filesystem operations can outlive task cancellation.

## Operator checks

**Admin → System → Capture journal**, the authenticated stats API, and Prometheus expose the journal state. Counter names use `llmtrace_journal_*_total`; gauges use `llmtrace_journal_*`.

| Signal | Meaning and action |
| --- | --- |
| `not_yet_durable` (API/UI), memory queue depth | Captures waiting for their first durable write. Sustained growth indicates admission is outrunning the journal writer. |
| `pending_records`, `pending_bytes`, `oldest_pending_age_secs` | Work waiting for replay/acknowledgement. Check database health, plugin failures, storage throughput, and capacity. |
| `dropped_full`, `write_failed` | New captures failed admission or synchronization. Inspect disk capacity/permissions and process logs; audit completeness has been affected. |
| `retry`, pipeline build/persist failures | Replay attempts failed; retained records retry. These counters alone do not mean a capture was permanently lost. |
| `read_failed`, `quarantined_records`, `quarantined_bytes` | Invalid records are preserved for investigation while healthy records continue. Temporary I/O read failures retry without quarantine. |
| `ack_failed` | Database work may have committed, but journal removal failed. Receipts prevent duplicate accounting on retry. |
| `blocked_records` | A pending record exceeds the current replay memory budget. |
| `recovered` | Startup-discovered records acknowledged during this process, including already-committed records whose deletion was interrupted. |

Counters reset on restart; pending/quarantine gauges are rebuilt from disk. Also monitor the existing memory/full/closed queue-drop counters. The UI request detail marks incomplete or interrupted payload capture; unknown-length streams cancelled before EOF are incomplete, while fully observed known-length responses (including HEAD) are complete even when the HTTP server does not poll EOF. These indicators describe what the proxy observed, not proof that an end client consumed every byte.

Malformed or interrupted journal files are renamed with a unique `.quarantine` suffix and are never automatically deleted. They count against capacity and block receipt cleanup for their trace ID. Stop the instance before manually inspecting or moving journal files; preserve the entire directory and associated receipts for investigation. There is no UI repair/delete action. After an operator removes resolved quarantine files while stopped, restart rebuilds the counters. Unindexed archive files from failed or changed writes are not automatically garbage-collected; stable retry paths reduce duplication but do not replace reconciliation.

## Verification and CI

Focused tests cover binary/metadata round trips, checksums including frame boundaries, private permissions, exclusive ownership, database binding, capacity reduction/exhaustion, quarantine preservation, and replay memory limits. Isolated PostgreSQL tests cover database write failures while journaling continues, concurrent replay idempotency, rollback after archive writes, and healthy replay alongside a corrupted record. A real HTTP test checks complete GET/HEAD responses and a cancelled SSE stream, including stored completeness flags. Subprocess tests kill a helper after a synchronized journal write and after database commit, then recover with a fresh pipeline; the latter also verifies that retention is not undone.

Run the functional suite with PostgreSQL available:

```sh
export DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/llmtrace
cargo test --workspace --locked -- --include-ignored --skip performance_ --skip journal_process_helper --skip openrouter_live
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Migration `006_trace_journal.sql` adds the receipt table and its journal lookup index. Migration tests use isolated databases; normal startup or `--migrate-only` applies it to a deployment.

[The CI workflow](../.github/workflows/ci.yml) builds embedded UI assets, checks formatting/lints, runs frontend and functional/database/crash tests, and applies migrations against PostgreSQL 16. A manual workflow input enables the three sequential release performance tests, including the browser. Adding the workflow does not mean it has executed on GitHub. See [journal performance results](journal-performance.md) for the local release run and [the earlier performance report](performance-testing.md) for the pre-journal baseline.
