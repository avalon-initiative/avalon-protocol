-- Real event batching (issue #38): a protocol event is never its own
-- settlement action. `ledger_entries` gains `batch_id`, grouping every
-- entry under the `EventBatch` it was committed with; `ledger_batches`
-- holds one row per batch (`first_seq`/`last_seq` bound its entries,
-- `batch_root` is the batch-level commitment `get_commitment` and `verify`
-- work from).
--
-- `batch_root` is a placeholder deterministic root — the batch's chain tip,
-- i.e. its last entry's `entry_hash` — not a Merkle root. Whether it becomes
-- one is issue #40's call; this migration only defines the column so the
-- shape is stable.
CREATE TABLE ledger_batches (
    batch_id UUID PRIMARY KEY,
    first_seq BIGINT NOT NULL,
    last_seq BIGINT NOT NULL,
    batch_root TEXT NOT NULL,
    committed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE ledger_entries ADD COLUMN batch_id UUID;

-- Backfill: every entry committed before real batching existed becomes one
-- synthetic batch, so batch_id can go NOT NULL with no gaps in history.
DO $$
DECLARE
    synthetic_batch_id UUID := gen_random_uuid();
    entries_first_seq BIGINT;
    entries_last_seq BIGINT;
    tip_hash TEXT;
BEGIN
    SELECT min(seq), max(seq) INTO entries_first_seq, entries_last_seq FROM ledger_entries;

    IF entries_first_seq IS NOT NULL THEN
        SELECT entry_hash INTO tip_hash FROM ledger_entries WHERE seq = entries_last_seq;

        INSERT INTO ledger_batches (batch_id, first_seq, last_seq, batch_root)
        VALUES (synthetic_batch_id, entries_first_seq, entries_last_seq, tip_hash);

        UPDATE ledger_entries SET batch_id = synthetic_batch_id;
    END IF;
END $$;

ALTER TABLE ledger_entries ALTER COLUMN batch_id SET NOT NULL;

-- Deferred to transaction end: `PostgresSettlementProvider::commit` inserts
-- a batch's entries before inserting the `ledger_batches` row itself (it
-- needs the entries' generated `seq` values to compute `first_seq`/
-- `last_seq` first), so the referenced row doesn't exist yet mid-transaction
-- — only by COMMIT time.
ALTER TABLE ledger_entries
    ADD CONSTRAINT ledger_entries_batch_id_fkey
    FOREIGN KEY (batch_id) REFERENCES ledger_batches(batch_id)
    DEFERRABLE INITIALLY DEFERRED;

CREATE INDEX ledger_entries_batch_id_idx ON ledger_entries(batch_id);
