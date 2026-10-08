CREATE TABLE indexer_identity_homes (
    identity_id TEXT NOT NULL,
    network_id TEXT NOT NULL,
    shard_id TEXT NOT NULL,
    PRIMARY KEY (identity_id, network_id, shard_id),
    CONSTRAINT indexer_identity_homes_identity_id_hex_chk CHECK (identity_id ~ '^[0-9a-f]{64}$')
);
