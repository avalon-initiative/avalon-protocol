-- Refuses to run over projected data: dedup claims become per shard and identity homes are
-- recorded when a creation is projected, so a populated projection would re-apply mirrored
-- history and refuse later key events. No backward compatibility: reset the dev database.
SET LOCAL lock_timeout = '10s';

DO $guard$
DECLARE
    t text;
    found boolean;
BEGIN
    IF coalesce(current_setting('avalon.allow_projection_reset', true), '') = 'on' THEN
        RETURN;
    END IF;
    FOREACH t IN ARRAY ARRAY['indexer_applied_events', 'mirrored_entries'] LOOP
        EXECUTE format('SELECT EXISTS (SELECT 1 FROM %I)', t) INTO found;
        IF found THEN
            RAISE EXCEPTION 'migration 0084 changes how projected events are claimed and where identities are homed (% is not empty). On a dev database run `make db-reset`. On any other database back it up first, then re-run with PGOPTIONS="-c avalon.allow_projection_reset=on" and rebuild the index.', t;
        END IF;
    END LOOP;
END
$guard$;

-- A signing key is registered once per identity: the same public key cannot reappear under a
-- new key id (which would resurrect a revoked key).
CREATE UNIQUE INDEX indexer_identity_signing_keys_identity_public_key_idx
    ON indexer_identity_signing_keys (identity_id, public_key);

-- Why a mirrored entry was not projected; refused entries are not rescanned.
ALTER TABLE mirrored_entries ADD COLUMN projection_rejection TEXT;

-- The shards where an identity's own signed creation was projected: the only non-core shards
-- whose key events are accepted for it.
CREATE TABLE indexer_identity_homes (
    identity_id TEXT NOT NULL,
    network_id TEXT NOT NULL,
    shard_id TEXT NOT NULL,
    PRIMARY KEY (identity_id, network_id, shard_id),
    CONSTRAINT indexer_identity_homes_identity_id_hex_chk CHECK (identity_id ~ '^[0-9a-f]{64}$')
);

-- An event id is claimed per shard, so one shard's copy cannot suppress another's.
ALTER TABLE indexer_applied_events ADD COLUMN shard_id TEXT NOT NULL DEFAULT '';
ALTER TABLE indexer_applied_events DROP CONSTRAINT indexer_applied_events_pkey;
ALTER TABLE indexer_applied_events ADD PRIMARY KEY (event_id, shard_id);
