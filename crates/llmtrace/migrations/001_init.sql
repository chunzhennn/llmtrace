CREATE TABLE IF NOT EXISTS request_traces (
    id uuid PRIMARY KEY,
    started_at timestamptz NOT NULL,
    completed_at timestamptz,
    method text NOT NULL,
    original_uri text NOT NULL,
    upstream_url text NOT NULL,
    upstream_host text,
    status integer,
    error text,
    request_kind text NOT NULL DEFAULT 'generic_http',
    model text,
    api_key_hash text,
    session_key text,
    session_id uuid,
    ttft_ms bigint,
    duration_ms bigint,
    bytes_in bigint NOT NULL DEFAULT 0,
    bytes_out bigint NOT NULL DEFAULT 0,
    request_headers jsonb NOT NULL DEFAULT '{}'::jsonb,
    response_headers jsonb NOT NULL DEFAULT '{}'::jsonb,
    request_body_compressed bytea NOT NULL DEFAULT ''::bytea,
    response_body_compressed bytea NOT NULL DEFAULT ''::bytea,
    request_body_bytes bigint NOT NULL DEFAULT 0,
    response_body_bytes bigint NOT NULL DEFAULT 0,
    request_body_truncated boolean NOT NULL DEFAULT false,
    response_body_truncated boolean NOT NULL DEFAULT false,
    content_type text,
    plugin_metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    tags text[] NOT NULL DEFAULT '{}'
);

CREATE INDEX IF NOT EXISTS idx_request_traces_started_at ON request_traces (started_at DESC);
CREATE INDEX IF NOT EXISTS idx_request_traces_status ON request_traces (status);
CREATE INDEX IF NOT EXISTS idx_request_traces_upstream_host ON request_traces (upstream_host);
CREATE INDEX IF NOT EXISTS idx_request_traces_model ON request_traces (model);
CREATE INDEX IF NOT EXISTS idx_request_traces_api_key_hash ON request_traces (api_key_hash);
CREATE INDEX IF NOT EXISTS idx_request_traces_session_id ON request_traces (session_id);
CREATE INDEX IF NOT EXISTS idx_request_traces_duration ON request_traces (duration_ms);
CREATE INDEX IF NOT EXISTS idx_request_traces_plugin_metadata ON request_traces USING GIN (plugin_metadata);
CREATE INDEX IF NOT EXISTS idx_request_traces_tags ON request_traces USING GIN (tags);

CREATE TABLE IF NOT EXISTS trace_rollups_minute (
    bucket timestamptz PRIMARY KEY,
    last_seen timestamptz NOT NULL,
    total bigint NOT NULL DEFAULT 0,
    errors bigint NOT NULL DEFAULT 0,
    captured_bytes bigint NOT NULL DEFAULT 0,
    duration_count bigint NOT NULL DEFAULT 0,
    duration_sum_ms bigint NOT NULL DEFAULT 0,
    ttft_count bigint NOT NULL DEFAULT 0,
    ttft_sum_ms bigint NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_trace_rollups_minute_last_seen ON trace_rollups_minute (last_seen DESC);

CREATE TABLE IF NOT EXISTS trace_sessions (
    id uuid PRIMARY KEY,
    session_key text NOT NULL UNIQUE,
    first_seen timestamptz NOT NULL,
    last_seen timestamptz NOT NULL,
    user_id text,
    user_name text,
    summary jsonb NOT NULL DEFAULT '{}'::jsonb
);

CREATE INDEX IF NOT EXISTS idx_trace_sessions_last_seen ON trace_sessions (last_seen DESC);
CREATE INDEX IF NOT EXISTS idx_trace_sessions_user_name ON trace_sessions (user_name);

CREATE TABLE IF NOT EXISTS session_messages (
    id bigserial PRIMARY KEY,
    request_id uuid NOT NULL REFERENCES request_traces(id) ON DELETE CASCADE,
    session_id uuid NOT NULL REFERENCES trace_sessions(id) ON DELETE CASCADE,
    role text NOT NULL,
    content text NOT NULL,
    created_at timestamptz NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_session_messages_session ON session_messages (session_id, created_at, id);
CREATE INDEX IF NOT EXISTS idx_session_messages_request ON session_messages (request_id);

CREATE TABLE IF NOT EXISTS ui_sessions (
    id text PRIMARY KEY,
    user_id text NOT NULL,
    display_name text NOT NULL,
    login_method text NOT NULL,
    created_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_ui_sessions_expires_at ON ui_sessions (expires_at);

CREATE TABLE IF NOT EXISTS ui_audit_events (
    id bigserial PRIMARY KEY,
    created_at timestamptz NOT NULL DEFAULT now(),
    event_type text NOT NULL,
    user_id text,
    remote_addr text,
    detail jsonb NOT NULL DEFAULT '{}'::jsonb
);

CREATE INDEX IF NOT EXISTS idx_ui_audit_created_at ON ui_audit_events (created_at DESC);
CREATE INDEX IF NOT EXISTS idx_ui_audit_event_type ON ui_audit_events (event_type);

CREATE TABLE IF NOT EXISTS oauth_states (
    state text PRIMARY KEY,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_oauth_states_expires_at ON oauth_states (expires_at);

CREATE OR REPLACE VIEW trace_requests AS
SELECT
    id, started_at, completed_at, method, original_uri, upstream_url, upstream_host,
    status, error, request_kind, model, api_key_hash, session_key, session_id,
    ttft_ms, duration_ms, bytes_in, bytes_out, request_headers, response_headers,
    request_body_bytes, response_body_bytes, request_body_truncated, response_body_truncated,
    content_type, plugin_metadata, tags
FROM request_traces;

CREATE OR REPLACE VIEW trace_messages AS
SELECT m.*
FROM session_messages m;
