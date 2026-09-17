-- Issue #525 (surfaced while implementing Part 2 of #521's decision):
-- mirror-only-node-reconstructible projection for event-signing keys,
-- exactly the `indexer_identity_passkeys`/#523 pattern applied to
-- `identity_signing_keys` instead. A live node keeps writing
-- `identity_signing_keys` directly, unchanged; this table is what
-- `identity.signing_key_added`/`.signing_key_revoked` replay into on any
-- node (authoring nodes dual-write it too, same as `indexer_identity_passkeys`),
-- and what session-continuation token verification reads from — a
-- continuation token must be checkable without assuming the verifying node
-- ever locally ran the ceremony that created the key.
CREATE TABLE indexer_identity_signing_keys (
    signing_key_id UUID PRIMARY KEY,
    identity_id UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    public_key BYTEA NOT NULL,
    label TEXT,
    added_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ
);

CREATE INDEX indexer_identity_signing_keys_identity_id_idx
    ON indexer_identity_signing_keys (identity_id);

-- Issue #525's anti-replay gate for session-continuation tokens: a
-- self-signed assertion is only ever accepted once. `expires_at` mirrors
-- the token's own claimed expiry so a cheap opportunistic sweep (delete
-- expired rows before inserting a new one) keeps this table from growing
-- unbounded without needing a separate scheduled job.
CREATE TABLE consumed_continuation_nonces (
    nonce UUID PRIMARY KEY,
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX consumed_continuation_nonces_expires_at_idx
    ON consumed_continuation_nonces (expires_at);
