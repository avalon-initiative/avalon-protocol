DROP INDEX ledger_entries_prunable_idx;
ALTER TABLE ledger_entries DROP COLUMN payload_pruned_at;
ALTER TABLE ledger_entries ALTER COLUMN payload SET NOT NULL;
