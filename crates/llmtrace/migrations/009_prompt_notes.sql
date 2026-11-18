CREATE TABLE note_index_settings (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    active_version integer NOT NULL CHECK (active_version > 0),
    backfill_enabled boolean NOT NULL DEFAULT true
);
INSERT INTO note_index_settings(singleton, active_version) VALUES (true, 1);

CREATE TABLE note_values (
    id bigserial PRIMARY KEY,
    index_version integer NOT NULL CHECK (index_version > 0),
    value_sha256 bytea NOT NULL CHECK (octet_length(value_sha256) = 32),
    value text NOT NULL,
    value_bytes integer NOT NULL CHECK (value_bytes BETWEEN 0 AND 4096),
    CHECK (octet_length(value) = value_bytes),
    UNIQUE (index_version, value_sha256),
    UNIQUE (id, index_version)
);

CREATE TABLE request_prompt_index_state (
    request_id uuid NOT NULL REFERENCES request_traces(id) ON DELETE CASCADE,
    index_version integer NOT NULL CHECK (index_version > 0),
    state text NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'processing', 'ready', 'partial', 'unsupported', 'failed')),
    capture_complete boolean NOT NULL,
    external_history boolean NOT NULL DEFAULT false,
    diagnostics jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(diagnostics) = 'array'),
    indexed_at timestamptz,
    attempt_count integer NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    next_retry_at timestamptz NOT NULL DEFAULT now(),
    priority smallint NOT NULL DEFAULT 0 CHECK (priority IN (0, 1)),
    lease_until timestamptz,
    lease_token uuid,
    PRIMARY KEY (request_id, index_version),
    CHECK ((state = 'processing') = (lease_until IS NOT NULL AND lease_token IS NOT NULL)),
    CHECK (state = 'processing' OR (lease_until IS NULL AND lease_token IS NULL))
);
CREATE INDEX idx_prompt_index_pending ON request_prompt_index_state (priority, next_retry_at, request_id)
    WHERE state IN ('pending', 'processing');

CREATE TABLE request_notes (
    id uuid PRIMARY KEY,
    request_id uuid NOT NULL,
    index_version integer NOT NULL,
    field_ordinal integer NOT NULL CHECK (field_ordinal >= 0),
    match_ordinal integer NOT NULL CHECK (match_ordinal >= 0),
    source_path text NOT NULL,
    source_kind text NOT NULL CHECK (source_kind IN ('message', 'system', 'instructions', 'tool_result', 'prompt')),
    role text NOT NULL,
    note_value_id bigint NOT NULL,
    decoded_start_byte integer NOT NULL CHECK (decoded_start_byte >= 0),
    decoded_end_byte integer NOT NULL CHECK (decoded_end_byte >= decoded_start_byte),
    FOREIGN KEY (request_id, index_version) REFERENCES request_prompt_index_state ON DELETE CASCADE,
    FOREIGN KEY (note_value_id, index_version) REFERENCES note_values(id, index_version) ON DELETE RESTRICT,
    UNIQUE (request_id, index_version, field_ordinal, match_ordinal)
);
CREATE INDEX idx_request_notes_value ON request_notes(note_value_id, request_id);

CREATE VIEW trace_prompt_notes AS
SELECT n.id, n.request_id, r.session_id, r.started_at AS observed_at, n.role,
       n.source_path, n.source_kind, n.field_ordinal, n.match_ordinal,
       n.decoded_start_byte, n.decoded_end_byte, v.value AS note_value,
       n.index_version, r.model, r.request_kind, r.api_key_hash,
       s.user_id, s.user_name, i.capture_complete, i.external_history,
       i.state AS index_state, v.value_sha256 AS note_value_sha256
FROM request_notes n
JOIN note_values v ON (v.id, v.index_version) = (n.note_value_id, n.index_version)
JOIN request_traces r ON r.id = n.request_id
LEFT JOIN trace_sessions s ON s.id = r.session_id
JOIN request_prompt_index_state i ON (i.request_id, i.index_version) = (n.request_id, n.index_version);

-- pg_trgm is optional and installed in an explicit non-transactional maintenance
-- step. Exact NOTE matching works without extensions or superuser privileges.
