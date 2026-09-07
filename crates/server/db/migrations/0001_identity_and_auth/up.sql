-- Identity, profile, credential, and session storage for milestone-1 auth.
-- Password/session mechanics are server-side implementation detail — not
-- part of the protocol domain model in crates/protocol, which only knows
-- about Identity and Profile as pure types.

CREATE TABLE identities (
    id UUID PRIMARY KEY,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE profiles (
    identity_id UUID PRIMARY KEY REFERENCES identities(id) ON DELETE CASCADE,
    display_name TEXT NOT NULL,
    avatar_url TEXT
);

-- Username/password is the milestone-1 credential mechanism, mirroring
-- WorldZero's own UsernamePasswordProvider approach for consistency between
-- the two related projects. Not modeled in crates/protocol on purpose: it's
-- one possible AuthProvider-style mechanism, not a protocol-level concept.
CREATE TABLE credentials (
    identity_id UUID PRIMARY KEY REFERENCES identities(id) ON DELETE CASCADE,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL
);

-- Opaque bearer tokens, not JWTs — session state lives here, not encoded in
-- the token itself, so a session can be revoked by deleting its row.
CREATE TABLE sessions (
    token TEXT PRIMARY KEY,
    identity_id UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX sessions_identity_id_idx ON sessions(identity_id);
