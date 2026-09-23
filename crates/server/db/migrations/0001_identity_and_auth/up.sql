-- Identity, profile, and authentication storage for milestone-1 identity.
--
-- Avalon identity is a self-custodied keypair, not a username/password
-- account (issue #73) — there is no server-side secret to leak, hash, or
-- steal, because there is no shared secret at all. Two independent keys per
-- identity, two independent jobs, mirroring how passkey-based crypto wallets
-- actually work in practice (the passkey is a secure "unlock," a separate
-- raw key is the actual signer):
--
--   * `identity_keys`         — WebAuthn passkeys. Interactive login only.
--   * `identity_signing_keys` — a raw Ed25519 key. Signs the durable
--                                protocol events this identity authors
--                                (starting with `identity.created`), so a
--                                hosted node can never fabricate an identity
--                                that never actually registered — the
--                                ledger entry itself carries a signature any
--                                mirror can verify independently of trusting
--                                the node. See docs/architecture/identity.md
--                                and docs/architecture/security-model.md.
--
-- Neither key is modeled in crates/protocol on purpose — this is server-side
-- implementation detail, not a protocol-level concept, same reasoning the
-- original username/password design already documented here.

CREATE TABLE identities (
    -- Client-chosen, not server-assigned: identity is a wallet the holder
    -- creates themselves, not an account the server hands out.
    id UUID PRIMARY KEY,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE profiles (
    identity_id UUID PRIMARY KEY REFERENCES identities(id) ON DELETE CASCADE,
    display_name TEXT NOT NULL,
    avatar_url TEXT
);

-- WebAuthn passkey credentials. Multiple per identity is supported from day
-- one (schema-level support for issue #99's cheapest recovery mitigation —
-- register a second device — even though the endpoint to add one isn't
-- built yet). `passkey_data` is webauthn-rs's own serialized `Passkey`
-- (public key, counter, transports, backup state — everything needed to
-- verify a future authentication and detect a cloned authenticator).
-- `credential_id` is duplicated out as its own indexed column because
-- discoverable/usernameless login needs to look a credential up by it
-- directly.
CREATE TABLE identity_keys (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    identity_id UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    credential_id BYTEA UNIQUE NOT NULL,
    passkey_data JSONB NOT NULL,
    label TEXT,
    added_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX identity_keys_identity_id_idx ON identity_keys(identity_id);

-- Raw Ed25519 signing keys. Losing this key alone is not catastrophic the
-- way losing every passkey is (issue #99) — the identity can still log in,
-- and can rotate to a new signing key from an authenticated session without
-- needing the old one (a later piece of work, not built yet). Historical
-- events stay verifiable against whichever key was valid when they were
-- signed, same principle as issuer key lifecycle (#80/#84).
CREATE TABLE identity_signing_keys (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    identity_id UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    public_key BYTEA NOT NULL,
    algorithm TEXT NOT NULL DEFAULT 'ed25519',
    label TEXT,
    added_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX identity_signing_keys_identity_id_idx ON identity_signing_keys(identity_id);

-- Ephemeral state between a WebAuthn ceremony's start and finish calls
-- (webauthn-rs requires the `PasskeyRegistration`/`DiscoverableAuthentication`
-- value returned from `start_*` to be handed back to the matching `finish_*`
-- call; this is a stateless HTTP API, so that value has to be persisted
-- somewhere in between). Short-lived by design — a lazy `expires_at` check
-- plus opportunistic delete-on-finish is all this needs, same simplicity
-- level as the `sessions` table below.
CREATE TABLE webauthn_ceremonies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    kind TEXT NOT NULL CHECK (kind IN ('registration', 'authentication')),
    state JSONB NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL
);

-- Opaque bearer tokens, not JWTs — session state lives here, not encoded in
-- the token itself, so a session can be revoked by deleting its row.
-- Unchanged by the move away from username/password: this table never had
-- anything to do with how an identity proves itself, only with what happens
-- after it does.
CREATE TABLE sessions (
    token TEXT PRIMARY KEY,
    identity_id UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX sessions_identity_id_idx ON sessions(identity_id);
