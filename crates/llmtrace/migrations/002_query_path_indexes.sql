CREATE INDEX IF NOT EXISTS idx_session_messages_created_at
ON session_messages (created_at DESC, id DESC);
