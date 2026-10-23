DELETE FROM indexer_identity_passkey_revocations a USING indexer_identity_passkey_revocations b
    WHERE a.ctid < b.ctid AND a.identity_id = b.identity_id AND a.passkey_id = b.passkey_id;
ALTER TABLE indexer_identity_passkey_revocations
    DROP CONSTRAINT indexer_identity_passkey_revocations_pkey;
ALTER TABLE indexer_identity_passkey_revocations DROP COLUMN event_id;
ALTER TABLE indexer_identity_passkey_revocations ADD PRIMARY KEY (identity_id, passkey_id);
DELETE FROM indexer_identity_signing_key_revocations a USING indexer_identity_signing_key_revocations b
    WHERE a.ctid < b.ctid AND a.identity_id = b.identity_id AND a.signing_key_id = b.signing_key_id;
ALTER TABLE indexer_identity_signing_key_revocations
    DROP CONSTRAINT indexer_identity_signing_key_revocations_pkey;
ALTER TABLE indexer_identity_signing_key_revocations DROP COLUMN event_id;
ALTER TABLE indexer_identity_signing_key_revocations ADD PRIMARY KEY (identity_id, signing_key_id);
ALTER TABLE indexer_identity_passkeys ADD COLUMN revoked_by_event_id UUID;
ALTER TABLE indexer_identity_signing_keys ADD COLUMN revoked_by_event_id UUID;
DROP INDEX identity_chain_state_anchor_dependent_idx;
ALTER TABLE identity_chain_state DROP COLUMN anchor_dependent;
ALTER TABLE identity_chain_events DROP COLUMN projected;
