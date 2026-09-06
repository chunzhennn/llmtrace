-- File removal happens after commit. Persist the work so crashes and temporary
-- filesystem failures cannot silently strand files after deleting their index.
CREATE TABLE archive_file_deletions (
    storage_key text PRIMARY KEY,
    compressed_bytes bigint NOT NULL,
    queued_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX idx_archive_file_deletions_queued ON archive_file_deletions (queued_at, storage_key);
CREATE INDEX idx_payload_archive_records_segment ON payload_archive_records (segment_id, trace_id);
CREATE INDEX idx_request_traces_rotation ON request_traces (started_at, id);
-- Legacy inline payloads are usually absent. Avoid scanning the entire trace
-- table every time size retention checks for them.
CREATE INDEX idx_request_traces_inline_payload_size ON request_traces
    ((octet_length(request_body_compressed)::bigint + octet_length(response_body_compressed)::bigint))
    WHERE octet_length(request_body_compressed) > 0 OR octet_length(response_body_compressed) > 0;

CREATE FUNCTION queue_deleted_archive_file() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.storage_backend = 'filesystem' THEN
        INSERT INTO archive_file_deletions (storage_key, compressed_bytes)
        VALUES (OLD.storage_key, OLD.compressed_bytes)
        ON CONFLICT (storage_key) DO NOTHING;
    END IF;
    RETURN OLD;
END;
$$;
CREATE TRIGGER queue_deleted_archive_file BEFORE DELETE ON payload_archive_segments
    FOR EACH ROW EXECUTE FUNCTION queue_deleted_archive_file();

-- Historical segments can contain multiple requests. Keep the segment until
-- its last record goes away; its PostgreSQL blob then cascades automatically.
CREATE FUNCTION delete_unreferenced_archive_segment() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM payload_archive_segments s
    WHERE s.id = OLD.segment_id
      AND NOT EXISTS (SELECT 1 FROM payload_archive_records r WHERE r.segment_id = s.id);
    RETURN OLD;
END;
$$;
CREATE TRIGGER delete_unreferenced_archive_segment AFTER DELETE ON payload_archive_records
    FOR EACH ROW EXECUTE FUNCTION delete_unreferenced_archive_segment();

-- Subtract atomically from the same bucket that insertion updates. This also
-- handles an age-retention batch that only removes part of a minute.
CREATE FUNCTION remove_deleted_trace_from_rollup() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    UPDATE trace_rollups_minute
    SET last_seen = CASE WHEN last_seen = OLD.started_at THEN COALESCE((
            SELECT MAX(started_at) FROM request_traces
            WHERE started_at >= date_trunc('minute', OLD.started_at)
              AND started_at < date_trunc('minute', OLD.started_at) + interval '1 minute'
        ), last_seen) ELSE last_seen END,
        total = GREATEST(0, total - 1),
        errors = GREATEST(0, errors - CASE WHEN OLD.error IS NOT NULL OR OLD.status >= 400 THEN 1 ELSE 0 END),
        captured_bytes = GREATEST(0, captured_bytes - OLD.request_body_bytes - OLD.response_body_bytes),
        duration_count = GREATEST(0, duration_count - CASE WHEN OLD.duration_ms IS NOT NULL THEN 1 ELSE 0 END),
        duration_sum_ms = GREATEST(0, duration_sum_ms - COALESCE(OLD.duration_ms, 0)),
        ttft_count = GREATEST(0, ttft_count - CASE WHEN OLD.ttft_ms IS NOT NULL THEN 1 ELSE 0 END),
        ttft_sum_ms = GREATEST(0, ttft_sum_ms - COALESCE(OLD.ttft_ms, 0))
    WHERE bucket = date_trunc('minute', OLD.started_at);
    DELETE FROM trace_rollups_minute
    WHERE bucket = date_trunc('minute', OLD.started_at) AND total = 0;
    RETURN OLD;
END;
$$;
CREATE TRIGGER remove_deleted_trace_from_rollup AFTER DELETE ON request_traces
    FOR EACH ROW EXECUTE FUNCTION remove_deleted_trace_from_rollup();
