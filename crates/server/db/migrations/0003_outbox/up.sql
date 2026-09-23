-- The outbox pattern fixing issue #71: an identity/profile/key write and its
-- durable protocol event must land together or not at all. Previously the
-- app-data transaction committed, then a *separate* call to
-- avalon-chain's `SettlementProvider::commit` happened after the fact — a
-- crash between the two left an identity with no corresponding ledger
-- entry, silently breaking "every identity has a durable creation record."
--
-- The fix: the event is inserted into this table in the *same* transaction
-- as the app-data rows it accompanies, before any external ledger call ever
-- happens. A background worker (crates/server/src/outbox.rs) then drains
-- pending rows into `avalon-chain` at its own pace and marks them
-- `committed_at`. A row that never gets picked up is not data loss — it is
-- durably recorded right here and can always be retried.
--
-- One `event JSONB` column, not flattened columns: `ProtocolEvent` is
-- already `Serialize`, so this is a straight round-trip with no schema
-- drift risk as the event shape evolves (see docs/architecture/protocol-events.md,
-- issue #82). `batch_id` is reserved for real event batching (issue #38) —
-- unused until then, always NULL.
CREATE TABLE protocol_outbox (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    event JSONB NOT NULL,
    enqueued_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    committed_at TIMESTAMPTZ,
    batch_id UUID
);

-- The worker's whole query shape is "pending rows, oldest first" — this is
-- the one index that matters.
CREATE INDEX protocol_outbox_pending_idx ON protocol_outbox (enqueued_at)
    WHERE committed_at IS NULL;
