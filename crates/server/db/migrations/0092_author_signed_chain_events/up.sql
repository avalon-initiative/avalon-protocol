-- A passkey's `identity.passkey_registered` event is signed by its owner in a second step, so the
-- passkey exists (and can log in on this node) before the event is announced.
ALTER TABLE identity_keys ADD COLUMN announced_at TIMESTAMPTZ;

-- A recovery names the new Ed25519 key the recovered identity will sign with (its key id is the
-- request id); each guardian approval carries the guardian's own signed approval of that request.
ALTER TABLE recovery_requests ADD COLUMN new_signing_public_key TEXT;
ALTER TABLE recovery_approvals ADD COLUMN signing_key_id UUID;
ALTER TABLE recovery_approvals ADD COLUMN signature TEXT;
