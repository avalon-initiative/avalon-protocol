-- A signing key is registered once per identity: the same public key cannot reappear under a
-- new key id (which would resurrect a revoked key). Rows that already break this are collapsed
-- first, keeping a revoked one if any, else the earliest.
DELETE FROM indexer_identity_signing_keys k
USING (
    SELECT ctid AS row_id FROM (
        SELECT ctid, row_number() OVER (
            PARTITION BY identity_id, public_key
            ORDER BY (revoked_at IS NULL), added_at, signing_key_id
        ) AS rank
        FROM indexer_identity_signing_keys
    ) ranked WHERE rank > 1
) doomed
WHERE k.ctid = doomed.row_id;

CREATE UNIQUE INDEX indexer_identity_signing_keys_identity_public_key_idx
    ON indexer_identity_signing_keys (identity_id, public_key);

-- Why a mirrored entry was not projected; refused entries are not rescanned.
ALTER TABLE mirrored_entries ADD COLUMN projection_rejection TEXT;

-- The shards where an identity's own signed creation was projected: the only non-core shards
-- whose key events are accepted for it.
CREATE TABLE indexer_identity_homes (
    identity_id TEXT NOT NULL CHECK (identity_id ~ '^[0-9a-f]{64}$'),
    network_id TEXT NOT NULL,
    shard_id TEXT NOT NULL,
    PRIMARY KEY (identity_id, network_id, shard_id)
);

-- An event id is claimed per shard, so one shard's copy cannot suppress another's.
ALTER TABLE indexer_applied_events ADD COLUMN shard_id TEXT NOT NULL DEFAULT '';
ALTER TABLE indexer_applied_events DROP CONSTRAINT indexer_applied_events_pkey;
ALTER TABLE indexer_applied_events ADD PRIMARY KEY (event_id, shard_id);
