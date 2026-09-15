-- Issue #481 (per #479's decided ADR: network isolation is enforced by a
-- per-network issuer registration gate, not by binding network_id into
-- attestation/event signatures). Two tables:
--
-- `issuer_network_registrations` — which issuer public keys are admitted
-- to write on *this server's own network*. Deliberately not the same
-- table as `issuer_keys` (integrator key custody/rotation, #80/#84):
-- `issuer_keys` answers "is this key part of integrator X's valid key
-- history" with no network concept at all, since integrator registration
-- is already per-server; this table answers a different question layered
-- on top — "has this specific pubkey been explicitly admitted on this
-- network" — closing the gap that attestation/event signing bytes never
-- bind network_id. Keyed on the raw public key bytes, not
-- integrator_id/key_id, matching the registration wire shape (#476),
-- which is deliberately decoupled from the integrators/issuer_keys tables.
--
-- `issuer_registration_challenges` — a short-lived, single-use nonce for
-- the registration endpoint's proof-of-possession signature, same shape
-- as `integrator_challenges` (migration 0011) but scoped to a raw pubkey
-- rather than an integrator_id, since a registering key may not (yet, or
-- ever) belong to a registered integrator on this server.
CREATE TABLE issuer_network_registrations (
    issuer_pubkey BYTEA PRIMARY KEY,
    issuer_ref TEXT NOT NULL,
    registered_at TIMESTAMPTZ NOT NULL,
    -- Distinguishes an explicit POST /issuers/register call from the
    -- dev/int-only implicit auto-registration on first valid signed write
    -- (#481's admission-policy design) — purely informational, never
    -- read to gate anything.
    auto_registered BOOLEAN NOT NULL DEFAULT false
);

CREATE TABLE issuer_registration_challenges (
    id UUID PRIMARY KEY,
    issuer_pubkey BYTEA NOT NULL,
    nonce BYTEA NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL
);
