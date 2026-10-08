-- The chain hash of a signing-key event covers only what its signature covers (#1354), so the hash
-- of every stored key event and every chain position pointing at one changed with no
-- compatibility: a database holding chained events must be wiped, not migrated.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM identity_chain_events) OR EXISTS (SELECT 1 FROM identity_chain_state WHERE seq > 0) THEN
        RAISE EXCEPTION 'migration 0089 changes the identity chain hash of key events (#1354); chain data written before it cannot verify, so wipe this database (make db-reset) before migrating';
    END IF;
END $$;

-- One signed event has one hash: a copy under another event id is the same chain event.
ALTER TABLE identity_chain_events DROP CONSTRAINT identity_chain_events_pkey;
ALTER TABLE identity_chain_events ADD PRIMARY KEY (identity_id, event_hash);
