-- Issuer registrations and keys per delivering stream, for verifying attestation revocations.
-- No foreign keys: events of one stream may arrive in any order.
CREATE TABLE indexer_issuers (
    network_id TEXT NOT NULL,
    shard_id TEXT NOT NULL,
    integrator_id UUID NOT NULL,
    issuer_ref TEXT NOT NULL,
    PRIMARY KEY (network_id, shard_id, integrator_id),
    UNIQUE (network_id, shard_id, issuer_ref)
);

CREATE TABLE indexer_issuer_keys (
    network_id TEXT NOT NULL,
    shard_id TEXT NOT NULL,
    key_id UUID NOT NULL,
    integrator_id UUID NOT NULL,
    algorithm TEXT NOT NULL,
    public_key BYTEA NOT NULL,
    role TEXT NOT NULL,
    purpose TEXT NOT NULL,
    valid_from TIMESTAMPTZ NOT NULL,
    valid_until TIMESTAMPTZ,
    PRIMARY KEY (network_id, shard_id, key_id)
);

CREATE INDEX indexer_issuer_keys_integrator_idx
    ON indexer_issuer_keys (network_id, shard_id, integrator_id);

CREATE TABLE indexer_issuer_key_revocations (
    network_id TEXT NOT NULL,
    shard_id TEXT NOT NULL,
    key_id UUID NOT NULL,
    revoked_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (network_id, shard_id, key_id)
);
