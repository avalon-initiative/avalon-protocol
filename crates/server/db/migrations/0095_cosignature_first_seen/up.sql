-- The earliest time a witness is known to have cosigned a head. A refresh moves observed_at (the
-- freshness clock) but never this, so the age of a ledger entry does not drift with refreshes.
ALTER TABLE witness_cosignatures ADD COLUMN first_seen_at TIMESTAMPTZ;
UPDATE witness_cosignatures SET first_seen_at = observed_at;
ALTER TABLE witness_cosignatures ALTER COLUMN first_seen_at SET NOT NULL;
