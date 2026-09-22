# Query editor

The admin Query page has **Builder** and **Code** modes. Builder provides dataset,
field, filter, sort and limit controls; Code provides a SQL-style statement editor
with syntax highlighting, inline diagnostics and schema-aware completion.

Switching from Builder to Code generates an equivalent statement. Valid Code
queries can be brought back into Builder, including plugin metadata projections,
filters and sort keys. Invalid statements never prevent switching: Builder keeps
its previous settings and Code retains the draft. Returning to Code without
changing Builder restores that draft. If Builder is edited, Code shows the updated
query and offers **Restore code draft** to recover the earlier statement.
**Query preview** shows the generated statement in
Builder without changing modes.

This interaction follows Grafana's
[Builder and Code query editors](https://grafana.com/docs/grafana/latest/datasources/postgres/query-editor/).
The implementation uses CodeMirror and translates the supported statement grammar
into the existing allowlisted `POST /api/query` request. SQL text is never sent to
Postgres or a raw SQL endpoint. No backend route, configuration or migration changes
are required.

## Write and run a query

```sql
SELECT id, model, status, duration_ms
FROM requests
WHERE status >= 400
ORDER BY started_at DESC
LIMIT 100;
```

- Start typing for suggestions; **Ctrl+Space** opens them explicitly.
- Use arrow keys to select, **Tab/Enter** to accept, and **Escape** to dismiss.
- **Enter** runs the query; **Shift+Enter** inserts a new line. While completion
  suggestions are open, **Enter** accepts the selected suggestion instead.
- The existing **Ctrl+Enter** / **Cmd+Enter** shortcuts also remain available.
- **Format query** formats a valid statement. Formatting removes comments.
- **Export** runs the current query and downloads its result rows as JSONL.

Suggestions come from `/api/query/schema`: datasets, available fields and types,
field-specific operators, clause keywords, sort directions and boolean values.
Plugin paths can be typed using `plugin_metadata.<plugin>.<field-path>` on the
requests dataset. Their dynamic path suffixes are not enumerated by the schema.

## Supported syntax

```text
SELECT field, field | *
FROM dataset
[WHERE field operator value [AND ...]]
[ORDER BY field [ASC | DESC], ...]
[LIMIT integer]
[;]
```

Datasets and fields are the names exposed by query schema discovery, such as
`requests`, `sessions`, `messages` and `rollups_minute`. `SELECT *` selects all
listed fields in that dataset. Supported operators are `=`, `!=` / `<>`, `>`,
`>=`, `<`, `<=`, `CONTAINS`, `IS NULL`, and `IS NOT NULL`, subject to the selected
field's allowed operators.

Use single quotes for text and double a quote to escape it: `'O''Reilly'`.
Boolean values are `TRUE` and `FALSE`; integer fields require safe JSON integers.
Timestamps must include a timezone, for example `'2026-09-22T00:00:00+08:00'`.
Explicit timestamp offsets and sub-millisecond precision survive mode switching.
Double-quote case-sensitive identifiers or identifiers with special characters:
`"plugin_metadata.My-plugin.team"`.

Use `JSON` followed by a quoted JSON literal for objects or arrays:

```sql
SELECT id, "plugin_metadata.identity.team"
FROM requests
WHERE plugin_metadata CONTAINS JSON '{"identity":{"team":"research"}}'
  AND "plugin_metadata.identity.team" IS NOT NULL
ORDER BY started_at DESC
LIMIT 100;
```

`CONTAINS` means a case-insensitive literal substring for text (no SQL wildcards),
JSON containment for JSON fields, or membership for a text-array field such as
`tags`. Use `IS NULL` for missing values. Standalone `JSON 'null'` is not supported
by the structured API and is rejected in both editor modes; null values inside
objects or arrays are supported.

If no sort is specified, the dataset's default order applies. An explicit
`ORDER BY field` defaults to ascending order. Omitted `LIMIT` defaults to 100;
explicit limits must be within the schema's range (currently 1–500).

This is a single-dataset selection grammar. Joins, OR, grouping/aggregations,
functions, subqueries, aliases and multiple statements are not supported. Unsupported
syntax is reported before execution. Use the structured API for automation;
query-result export is bounded by the limit, while full session export is described
in [session-export.md](session-export.md).

The editor preserves the current query while switching modes; it does not persist
query drafts across a page reload. Query execution and JSONL export use the existing
admin authentication and backend field/operator validation.
