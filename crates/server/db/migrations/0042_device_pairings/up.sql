-- Cross-device pairing (issue #307): lets a WebAuthn-incapable client (a
-- game engine, a console, a headless context) bootstrap a real session
-- without ever touching a password. `session_token` references `sessions`
-- rather than an `id`, since `sessions`' own primary key is the opaque
-- token itself (see 0001_identity_and_auth) — there is no separate session
-- id to point at. `identity_id` stays nullable until approval: which
-- identity a pairing belongs to is only known once some already-logged-in
-- session approves it.
CREATE TABLE device_pairings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    device_code TEXT UNIQUE NOT NULL,
    user_code TEXT UNIQUE NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'denied', 'expired')),
    identity_id UUID REFERENCES identities(id) ON DELETE CASCADE,
    session_token TEXT REFERENCES sessions(token) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    last_polled_at TIMESTAMPTZ
);

CREATE INDEX device_pairings_user_code_idx ON device_pairings(user_code);
