-- Reverts to plaintext-token sessions; existing sessions, pairings and login requests are dropped.
SET LOCAL lock_timeout = '10s';

DELETE FROM device_pairings;
DELETE FROM cross_node_login_requests;

ALTER TABLE device_pairings DROP COLUMN approved_by_signing_key_id;
ALTER TABLE device_pairings RENAME COLUMN device_code_hash TO device_code;
ALTER TABLE cross_node_login_requests DROP COLUMN approved_by_signing_key_id;
ALTER TABLE cross_node_login_requests RENAME COLUMN request_code_hash TO request_code;

DROP TABLE sessions;

CREATE TABLE sessions (
    token TEXT PRIMARY KEY,
    identity_id TEXT NOT NULL REFERENCES identities(id) ON DELETE CASCADE
        CONSTRAINT sessions_identity_id_hex_chk CHECK (identity_id ~ '^[0-9a-f]{64}$'),
    expires_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX sessions_identity_id_idx ON sessions(identity_id);

ALTER TABLE device_pairings ADD COLUMN session_token TEXT REFERENCES sessions(token) ON DELETE SET NULL;
ALTER TABLE cross_node_login_requests ADD COLUMN session_token TEXT REFERENCES sessions(token) ON DELETE SET NULL;
