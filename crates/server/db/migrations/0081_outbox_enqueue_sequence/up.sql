-- Rows enqueued in one transaction share `enqueued_at` (the transaction start),
-- so the drain needs a monotonic tiebreaker to keep their enqueue order.
ALTER TABLE protocol_outbox ADD COLUMN seq BIGINT GENERATED ALWAYS AS IDENTITY;
