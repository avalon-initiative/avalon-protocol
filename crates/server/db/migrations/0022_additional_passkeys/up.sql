-- Issue #200: an authenticated identity can register additional WebAuthn
-- passkeys, not just the one created at account registration. The
-- `identity_keys` table (migration 0001) already supports multiple rows per
-- identity, including a `label` column that register_finish never actually
-- populated — no new table needed, per docs/architecture/identity.md's own
-- note that this was schema-ready and just missing an endpoint.
--
-- The one real schema gap: `webauthn_ceremonies.kind` only allowed
-- 'registration' (identity creation, unauthenticated) and 'authentication'
-- (login). Reusing 'registration' for the add-a-passkey ceremony would let
-- an authenticated add-passkey ceremony's state be fed into the
-- unauthenticated `register_finish` (identity-creation) handler and vice
-- versa — both deserialize permissively (serde ignores unknown fields), so
-- the two flows need to be kept apart by construction, not by convention. A
-- distinct 'add_passkey' kind does that: `handlers::register_finish` only
-- ever consumes 'registration' rows, `passkeys::register_finish` only ever
-- consumes 'add_passkey' rows.
ALTER TABLE webauthn_ceremonies DROP CONSTRAINT webauthn_ceremonies_kind_check;
ALTER TABLE webauthn_ceremonies
    ADD CONSTRAINT webauthn_ceremonies_kind_check
    CHECK (kind IN ('registration', 'authentication', 'add_passkey'));
