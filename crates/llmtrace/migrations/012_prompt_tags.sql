-- No tag is enabled until an administrator chooses it. Old derived rows
-- have no tag identity and are rebuilt from their retained request bodies.
ALTER TABLE note_index_settings
    ADD COLUMN tag_names text[] NOT NULL DEFAULT '{}'::text[]
        CHECK (cardinality(tag_names) <= 32 AND array_position(tag_names, NULL) IS NULL),
    ADD COLUMN tag_revision bigint NOT NULL DEFAULT 1 CHECK (tag_revision > 0);
ALTER TABLE request_prompt_index_state ADD COLUMN tag_revision bigint NOT NULL DEFAULT 1;
ALTER TABLE note_index_coverage ADD COLUMN tag_revision bigint NOT NULL DEFAULT 1;
CREATE INDEX idx_prompt_index_tag_revision ON request_prompt_index_state(tag_revision, index_version);
ALTER TABLE request_notes ADD COLUMN tag_name text
    CHECK (tag_name COLLATE "C" ~ '^[A-Za-z][A-Za-z0-9_-]{0,31}$');

CREATE INDEX idx_request_notes_tag_value ON request_notes(tag_name, note_value_id, request_id)
    WHERE tag_name IS NOT NULL;

CREATE VIEW trace_prompt_tags AS
SELECT n.id, n.request_id, r.session_id, r.started_at AS observed_at, n.role,
       n.source_path, n.source_kind, n.field_ordinal, n.match_ordinal,
       n.decoded_start_byte, n.decoded_end_byte, v.value AS note_value,
       n.index_version, r.model, r.request_kind, r.api_key_hash,
       s.user_id, s.user_name, i.capture_complete, i.external_history,
       i.state AS index_state, v.value_sha256 AS note_value_sha256,
       n.tag_name, v.value AS tag_value, n.note_value_id, i.tag_revision
FROM request_notes n
JOIN note_values v ON (v.id, v.index_version) = (n.note_value_id, n.index_version)
JOIN request_traces r ON r.id = n.request_id
LEFT JOIN trace_sessions s ON s.id = r.session_id
JOIN request_prompt_index_state i ON (i.request_id, i.index_version) = (n.request_id, n.index_version)
-- Each job contains exactly the names from its revision. Legacy occurrences
-- have NULL names. Look up the active settings once per statement.
WHERE n.tag_name IS NOT NULL
  AND i.tag_revision = (SELECT tag_revision FROM note_index_settings WHERE singleton)
  AND n.index_version = (SELECT active_version FROM note_index_settings WHERE singleton);

CREATE OR REPLACE VIEW trace_prompt_notes AS
SELECT * FROM trace_prompt_tags;
