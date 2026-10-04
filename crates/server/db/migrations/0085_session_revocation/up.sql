-- Sessions are stored by SHA-256 of the bearer token and record the credential that produced
-- them, so revoking that credential can end them. Existing sessions are discarded (tokens cannot
-- be hashed in place without the plaintext), as are in-flight device pairings and cross-node
-- login requests, which held plaintext session tokens or codes.
SET LOCAL lock_timeout = '10s';

DELETE FROM device_pairings;
DELETE FROM cross_node_login_requests;

ALTER TABLE device_pairings DROP COLUMN session_token;
ALTER TABLE device_pairings RENAME COLUMN device_code TO device_code_hash;
ALTER TABLE device_pairings ADD COLUMN approved_by_signing_key_id UUID;

ALTER TABLE cross_node_login_requests DROP COLUMN session_token;
ALTER TABLE cross_node_login_requests RENAME COLUMN request_code TO request_code_hash;
ALTER TABLE cross_node_login_requests ADD COLUMN approved_by_signing_key_id UUID;

DROP TABLE sessions;

CREATE TABLE sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    token_hash BYTEA NOT NULL UNIQUE CHECK (length(token_hash) = 32),
    identity_id TEXT NOT NULL REFERENCES identities(id) ON DELETE CASCADE
        CONSTRAINT sessions_identity_id_hex_chk CHECK (identity_id ~ '^[0-9a-f]{64}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    -- The passkey (identity_keys.id) that authenticated this session, when it was a login.
    origin_passkey_id UUID,
    -- The signing key that approved this session (device pairing or cross-node grant).
    origin_signing_key_id UUID
);

CREATE INDEX sessions_identity_id_idx ON sessions(identity_id);
CREATE INDEX sessions_expires_at_idx ON sessions(expires_at);
CREATE INDEX sessions_origin_passkey_idx ON sessions(origin_passkey_id) WHERE origin_passkey_id IS NOT NULL;
CREATE INDEX sessions_origin_signing_key_idx ON sessions(origin_signing_key_id) WHERE origin_signing_key_id IS NOT NULL;
