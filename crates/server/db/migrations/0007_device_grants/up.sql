-- Device-registration / linked-device grant model for signing-key custody
-- (issue #135, decided in #122 as the primary path — #134's mnemonic
-- phrase is the fallback underneath it, not this).
--
-- No new `devices` table: a device *is* a row in `identity_signing_keys`
-- (already supports multiple rows per identity, already has `label`). A
-- grant only ever *authorizes a new public key* into that table — it never
-- transfers a private key across the network, extending #134's "server
-- only ever sees the public key" invariant per-device instead of weakening
-- it. See crates/server/src/devices.rs for the request/approve/revoke flow.
ALTER TABLE identity_signing_keys ADD COLUMN revoked_at TIMESTAMPTZ;

CREATE TABLE device_grants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    identity_id UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    requested_signing_public_key BYTEA NOT NULL,
    device_label TEXT,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'approved', 'denied', 'expired')),
    requested_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    approved_by_signing_key_id UUID REFERENCES identity_signing_keys(id),
    approved_at TIMESTAMPTZ
);

CREATE INDEX device_grants_identity_id_idx ON device_grants(identity_id);
