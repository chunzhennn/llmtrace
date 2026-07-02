CREATE TABLE IF NOT EXISTS payload_archive_segments (
    id uuid PRIMARY KEY,
    session_id uuid REFERENCES trace_sessions(id) ON DELETE CASCADE,
    segment_index bigint NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    sealed_at timestamptz,
    uncompressed_bytes bigint NOT NULL DEFAULT 0,
    compressed_bytes bigint NOT NULL DEFAULT 0,
    record_count bigint NOT NULL DEFAULT 0,
    compression_codec text NOT NULL DEFAULT 'zstd',
    compression_level integer NOT NULL,
    storage_backend text NOT NULL,
    storage_key text NOT NULL,
    checksum_sha256 text NOT NULL,
    sealed boolean NOT NULL DEFAULT false,
    UNIQUE (session_id, segment_index)
);

CREATE INDEX IF NOT EXISTS idx_payload_archive_segments_session
    ON payload_archive_segments (session_id, segment_index DESC);

CREATE TABLE IF NOT EXISTS payload_archive_segment_blobs (
    segment_id uuid PRIMARY KEY REFERENCES payload_archive_segments(id) ON DELETE CASCADE,
    compressed_payload bytea NOT NULL
);

CREATE TABLE IF NOT EXISTS payload_archive_records (
    id uuid PRIMARY KEY,
    trace_id uuid NOT NULL REFERENCES request_traces(id) ON DELETE CASCADE,
    session_id uuid REFERENCES trace_sessions(id) ON DELETE CASCADE,
    segment_id uuid NOT NULL REFERENCES payload_archive_segments(id) ON DELETE CASCADE,
    record_index bigint NOT NULL,
    direction text NOT NULL,
    content_type text,
    uncompressed_offset bigint NOT NULL,
    uncompressed_len bigint NOT NULL,
    body_sha256 text NOT NULL,
    complete boolean NOT NULL DEFAULT true,
    capture_error text,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (trace_id, direction)
);

CREATE INDEX IF NOT EXISTS idx_payload_archive_records_trace
    ON payload_archive_records (trace_id);

CREATE INDEX IF NOT EXISTS idx_payload_archive_records_session
    ON payload_archive_records (session_id, created_at);
