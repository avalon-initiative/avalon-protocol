-- Issue #523: `identity.passkey_registered`/`identity.passkey_revoked`
-- projection, built for a mirror-only node — a live node keeps writing
-- `identity_keys` directly, exactly as it does today, since that table is
-- its own local source of truth for a login it actually ran the WebAuthn
-- ceremony for. This projection exists so a node that only ever *mirrored*
-- an identity's ledger history (never ran that ceremony locally) can still
-- reconstruct a usable, verification-ready credential table from replayed
-- events alone — the ticket's own acceptance criterion.
--
-- Soft-revoked (`revoked_at`), unlike `identity_keys`' hard delete: nothing
-- here is the actual login-authority table on a live node, so there's no
-- harm in keeping revoked rows around for audit/rebuild-diff purposes, and
-- `crate::postgres::PROJECTION_TABLES`-driven rebuild needs a stable target
-- to truncate/replay into either way.
CREATE TABLE indexer_identity_passkeys (
    passkey_id UUID PRIMARY KEY,
    identity_id UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    credential_id BYTEA NOT NULL UNIQUE,
    passkey_data JSONB NOT NULL,
    label TEXT,
    added_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ
);

CREATE INDEX indexer_identity_passkeys_identity_id_idx
    ON indexer_identity_passkeys (identity_id);
