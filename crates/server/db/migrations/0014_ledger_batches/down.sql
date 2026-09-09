ALTER TABLE ledger_entries DROP CONSTRAINT ledger_entries_batch_id_fkey;
DROP INDEX ledger_entries_batch_id_idx;
ALTER TABLE ledger_entries DROP COLUMN batch_id;
DROP TABLE ledger_batches;
