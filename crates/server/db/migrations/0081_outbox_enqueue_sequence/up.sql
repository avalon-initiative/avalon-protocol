-- Rows enqueued in one transaction share `enqueued_at` (the transaction start),
-- so the drain needs a monotonic tiebreaker. Metadata-only (no table rewrite):
-- existing rows keep NULL, which sorts last among ties.
SET LOCAL lock_timeout = '10s';
ALTER TABLE protocol_outbox ADD COLUMN seq BIGINT;
CREATE SEQUENCE protocol_outbox_seq_seq OWNED BY protocol_outbox.seq;
ALTER TABLE protocol_outbox ALTER COLUMN seq SET DEFAULT nextval('protocol_outbox_seq_seq');
