-- The ledger entry hash commits to a SHA-256 of the canonical payload instead of the payload bytes
-- (#1226), so a row whose payload was pruned still verifies. The hash layout changed, so entries
-- written before this migration no longer verify (the empty hash marks them); dev databases are
-- wiped across this change.
ALTER TABLE ledger_entries ADD COLUMN payload_hash TEXT NOT NULL DEFAULT '';
ALTER TABLE ledger_entries ALTER COLUMN payload_hash DROP DEFAULT;

ALTER TABLE mirrored_entries ADD COLUMN payload_hash TEXT NOT NULL DEFAULT '';
ALTER TABLE mirrored_entries ALTER COLUMN payload_hash DROP DEFAULT;
