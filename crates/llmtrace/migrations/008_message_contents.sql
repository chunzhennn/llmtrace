-- Additive rollout: old writers still work and all inline data remains intact.
CREATE TABLE message_contents (
    id bigserial PRIMARY KEY,
    content_sha256 bytea NOT NULL UNIQUE CHECK (octet_length(content_sha256) = 32),
    content_bytes integer NOT NULL CHECK (content_bytes >= 0),
    content text NOT NULL,
    CHECK (octet_length(content) = content_bytes)
);

ALTER TABLE session_messages ADD COLUMN content_id bigint;
ALTER TABLE session_messages ADD CONSTRAINT session_messages_content_fk
    FOREIGN KEY (content_id) REFERENCES message_contents(id) ON DELETE RESTRICT NOT VALID;
ALTER TABLE session_messages ADD CONSTRAINT session_messages_content_present
    CHECK (content IS NOT NULL OR content_id IS NOT NULL) NOT VALID;
ALTER TABLE session_messages ALTER COLUMN content DROP NOT NULL;
-- Fresh databases can prepare empty indexes here. Existing tables use the
-- explicit `--message-maintenance prepare` command (CONCURRENTLY, outside the
-- embedded migration transaction) before backfill/cleanup.
DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM session_messages LIMIT 1) THEN
        CREATE INDEX idx_session_messages_content_id ON session_messages (content_id) WHERE content_id IS NOT NULL;
        CREATE INDEX idx_session_messages_uninterned ON session_messages (id) WHERE content_id IS NULL;
        ALTER TABLE session_messages VALIDATE CONSTRAINT session_messages_content_fk;
        ALTER TABLE session_messages VALIDATE CONSTRAINT session_messages_content_present;
    END IF;
END $$;

-- Preserve the original view's column order, including historical nullable state.
CREATE OR REPLACE VIEW trace_messages AS
SELECT m.id, m.request_id, m.session_id, m.role,
       COALESCE(m.content, c.content) AS content, m.created_at, m.content_truncated
FROM session_messages m LEFT JOIN message_contents c ON c.id = m.content_id;

CREATE TABLE message_storage_migration (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    write_mode text NOT NULL DEFAULT 'dual_write'
        CHECK (write_mode IN ('legacy', 'dual_write', 'references_only')),
    high_water_id bigint NOT NULL DEFAULT 0 CHECK (high_water_id >= 0),
    backfilled_rows bigint NOT NULL DEFAULT 0 CHECK (backfilled_rows >= 0),
    updated_at timestamptz NOT NULL DEFAULT now(),
    last_error text,
    verified_at timestamptz,
    verified_missing_references bigint,
    verified_dangling_references bigint,
    verified_mismatched_contents bigint,
    verified_missing_inline bigint,
    gates_acknowledged_at timestamptz
);
INSERT INTO message_storage_migration(singleton) VALUES (true);
