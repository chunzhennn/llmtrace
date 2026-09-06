-- Unknown usage and price remain NULL; historical bodies are not reparsed.
ALTER TABLE request_traces
    ADD COLUMN ttfb_ms bigint,
    ADD COLUMN input_tokens bigint,
    ADD COLUMN output_tokens bigint,
    ADD COLUMN cached_input_tokens bigint,
    ADD COLUMN cache_creation_input_tokens bigint,
    ADD COLUMN usage_complete boolean NOT NULL DEFAULT false,
    ADD COLUMN estimated_cost_microusd bigint,
    ADD COLUMN tool_calls jsonb NOT NULL DEFAULT '[]'::jsonb,
    ADD COLUMN tool_call_count bigint NOT NULL DEFAULT 0;

CREATE OR REPLACE VIEW trace_requests AS
SELECT
    id, started_at, completed_at, method, original_uri, upstream_url, upstream_host,
    status, error, request_kind, model, api_key_hash, session_key, session_id,
    ttft_ms, duration_ms, bytes_in, bytes_out, request_headers, response_headers,
    request_body_bytes, response_body_bytes, request_body_truncated, response_body_truncated,
    content_type, plugin_metadata, tags,
    ttfb_ms, input_tokens, output_tokens, cached_input_tokens, cache_creation_input_tokens, usage_complete, estimated_cost_microusd, tool_calls, tool_call_count
FROM request_traces;


-- Historical ttft_ms held time-to-first-byte. Preserve it under its true name.
UPDATE request_traces SET ttfb_ms = ttft_ms, ttft_ms = NULL WHERE ttft_ms IS NOT NULL;
-- Keep existing rollups consistent with corrected failure and timing semantics.
UPDATE trace_rollups_minute SET ttft_count = 0, ttft_sum_ms = 0;
UPDATE trace_rollups_minute r
SET errors = counts.errors
FROM (
    SELECT date_trunc('minute', started_at) AS bucket,
           COUNT(*) FILTER (WHERE error IS NOT NULL OR status >= 400) AS errors
    FROM request_traces GROUP BY 1
) counts WHERE r.bucket = counts.bucket;

CREATE INDEX idx_request_traces_session_started ON request_traces (session_id, started_at DESC, id DESC);
