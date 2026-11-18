CREATE TABLE note_index_coverage (
    index_version integer PRIMARY KEY CHECK (index_version > 0),
    checked_at timestamptz NOT NULL,
    pending bigint NOT NULL,
    processing bigint NOT NULL,
    ready bigint NOT NULL,
    partial bigint NOT NULL,
    unsupported bigint NOT NULL,
    failed bigint NOT NULL,
    missing_state bigint NOT NULL,
    missing_source bigint NOT NULL
);
