# Credential audit — 2026-09-22

The audit of commit `4911a6b` and all locally available history, after fetching
all remote branches and tags, found no real API keys, private keys, or matches
for the local configured secret checked during the audit. No history rewrite
was necessary.

Scope and checks:

- All 126 commits reachable through branches, tags, remote refs and reflogs.
- 719 distinct historical file blobs, including deleted file versions; no
  committed `.env`, `llmtrace.toml`, private-key files, logs or spool data were found.
- Gitleaks 8.30.1, verified against its release checksum and a synthetic-token
  positive control, scanned history with recursive decoding/archive scanning.
  Commit messages were scanned separately.
- An independent token/private-key pattern scan and an exact comparison against
  the local configured secret, without printing or storing that secret in reports.
- OCR of all 60 historical PNG screenshots. Token-like text in the redaction
  screenshots matched the application's built-in synthetic sample exactly.

After the CLI version, request ID copy, and user terminology changes, a follow-up
scan through commit `b39101c` covered all 129 reachable commits and found no leaks.
The pending release files and current tracked source tree also passed Gitleaks.

The three initial Gitleaks findings were README examples: `sk-example`,
`sk-ant-example`, and `conversation-456`. The latter is a conversation identifier
in a plugin-output example, not an authentication credential. `.gitleaks.toml`
excludes only these exact values, constrained to the README and matching rules;
it does not exclude entire commits, source directories or tests. Other checked
credentials were explicit local-development defaults or synthetic test values.

Verify CI now scans full history on branch pushes and pull requests. Release
builds depend on that scan. Logs redact any detected value. The scanner binary
version and checksum are pinned; no external scanning service receives source
or credentials.

To repeat the history scan with [Gitleaks](https://github.com/gitleaks/gitleaks):

```bash
git fetch --all --tags
gitleaks git . --config .gitleaks.toml --log-opts="--all --reflog" \
  --redact=100 --no-banner --max-decode-depth=3 --max-archive-depth=2
```

This records the inspected history and detection methods, not a guarantee about
unavailable remote refs, external logs, future commits, or every possible secret
format. Keep runtime configs and captured traffic out of Git; rotate and remove
any genuine credential reported by a later scan rather than allowlisting it.
