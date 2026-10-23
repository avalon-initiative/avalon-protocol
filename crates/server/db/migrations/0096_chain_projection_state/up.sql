-- Which recorded chain events are projected, so a node reverts and applies exactly the difference when
-- the chain it resolves changes, and which identities depend on cosigned head dates.
ALTER TABLE identity_chain_events ADD COLUMN projected BOOLEAN NOT NULL DEFAULT true;
ALTER TABLE identity_chain_events ALTER COLUMN projected SET DEFAULT false;
ALTER TABLE identity_chain_state ADD COLUMN anchor_dependent BOOLEAN NOT NULL DEFAULT false;
CREATE INDEX identity_chain_state_anchor_dependent_idx ON identity_chain_state (identity_id)
    WHERE anchor_dependent;

-- Every event that revoked a key or passkey is kept, so removing one revoker never restores a subject
-- another still revokes; the subject's revoked_at is the earliest of its revokers.
ALTER TABLE indexer_identity_signing_keys DROP COLUMN revoked_by_event_id;
ALTER TABLE indexer_identity_passkeys DROP COLUMN revoked_by_event_id;
ALTER TABLE indexer_identity_signing_key_revocations
    ADD COLUMN event_id UUID NOT NULL DEFAULT gen_random_uuid();
ALTER TABLE indexer_identity_signing_key_revocations
    DROP CONSTRAINT indexer_identity_signing_key_revocations_pkey;
ALTER TABLE indexer_identity_signing_key_revocations
    ADD PRIMARY KEY (identity_id, signing_key_id, event_id);
ALTER TABLE indexer_identity_passkey_revocations
    ADD COLUMN event_id UUID NOT NULL DEFAULT gen_random_uuid();
ALTER TABLE indexer_identity_passkey_revocations
    DROP CONSTRAINT indexer_identity_passkey_revocations_pkey;
ALTER TABLE indexer_identity_passkey_revocations
    ADD PRIMARY KEY (identity_id, passkey_id, event_id);
