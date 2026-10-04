-- A signing key is registered once per identity: the same public key cannot reappear under a
-- new key id (which would resurrect a revoked key). Mirror rows record why a projection was refused.
CREATE UNIQUE INDEX indexer_identity_signing_keys_identity_public_key_idx
    ON indexer_identity_signing_keys (identity_id, public_key);

ALTER TABLE mirrored_entries ADD COLUMN projection_rejection TEXT;
