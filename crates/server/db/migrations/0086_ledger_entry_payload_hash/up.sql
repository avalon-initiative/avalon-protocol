-- The ledger entry hash commits to a SHA-256 of the canonical payload instead of the payload bytes
-- (#1226), so a row whose payload was pruned still verifies. The hash layout changed with no
-- compatibility: a database holding entries must be wiped, not migrated.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM ledger_entries) OR EXISTS (SELECT 1 FROM mirrored_entries) THEN
        RAISE EXCEPTION 'migration 0086 changes the ledger entry hash format (#1226); entries written before it cannot verify, so wipe this database (make db-reset) before migrating';
    END IF;
END $$;

ALTER TABLE ledger_entries ADD COLUMN payload_hash TEXT NOT NULL CHECK (length(payload_hash) = 64);
ALTER TABLE mirrored_entries ADD COLUMN payload_hash TEXT NOT NULL CHECK (length(payload_hash) = 64);
