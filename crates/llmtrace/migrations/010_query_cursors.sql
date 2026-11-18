-- A shared random HMAC key keeps opaque cursors valid across server instances.
CREATE TABLE query_cursor_key (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    secret bytea NOT NULL CHECK (octet_length(secret) = 32)
);
INSERT INTO query_cursor_key VALUES (true,
    decode(replace(gen_random_uuid()::text || gen_random_uuid()::text, '-', ''), 'hex'));
