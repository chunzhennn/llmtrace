-- Nullable for historical previews: a request-wide tag cannot identify exactly
-- which individual message was shortened. No body reads or guesses in migration.
ALTER TABLE session_messages ADD COLUMN content_truncated boolean;

CREATE OR REPLACE VIEW trace_messages AS SELECT m.* FROM session_messages m;
