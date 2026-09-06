-- Kept independently of trace retention until the journal has durably removed
-- its record. This prevents replay from resurrecting already-rotated traces.
CREATE TABLE trace_ingest_receipts (
    trace_id UUID PRIMARY KEY,
    journal_id UUID NOT NULL,
    ingested_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX trace_ingest_receipts_journal ON trace_ingest_receipts (journal_id, trace_id);
