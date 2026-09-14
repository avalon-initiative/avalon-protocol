-- Issue #201: social recovery via an M-of-N set of trusted guardians —
-- the real answer #99 called for, complementing #200's multi-passkey
-- mitigation for losing all devices at once rather than just one.
--
-- Three concerns, three tables:
--
--   * `recovery_guardian_settings` — an identity's current threshold. One
--     row per identity that has ever configured guardians; absent/threshold
--     0 means recovery is not configured and initiation must be refused
--     (an M-of-0 scheme can never be satisfied, and a guardian-less
--     identity has no one to ask).
--   * `recovery_guardians` — the guardian set itself, drawn from the
--     user's friends (enforced at the handler layer against
--     `friendships`, not by a DB constraint — friendship can change after
--     a guardian is designated, and a former friend remaining a guardian
--     until the owner explicitly changes the set is the safer default,
--     matching #200/#135's "no cooperation required to revoke" precedent).
--   * `recovery_requests` / `recovery_approvals` — one in-flight (or
--     historical) recovery attempt per row, and the per-guardian approvals
--     against it. The new device's WebAuthn passkey is captured into
--     `pending_passkey_data`/`pending_credential_id` at request-completion
--     time (same ceremony shape as `identity_keys`) but is not written into
--     `identity_keys` until the request actually finalizes — exactly the
--     "no new credential is a valid login credential until the delay
--     clears with no veto" invariant #201 requires.
--
-- Changing the guardian set requires the identity's *current* session
-- (enforced by `authenticate` in the handler, same as every other
-- session-gated route) — never reachable mid-recovery-attempt by an
-- attacker who has compromised only the new, not-yet-valid device.

CREATE TABLE recovery_guardian_settings (
    identity_id UUID PRIMARY KEY REFERENCES identities(id) ON DELETE CASCADE,
    threshold INT NOT NULL CHECK (threshold >= 1),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE recovery_guardians (
    identity_id UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    guardian_identity_id UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    added_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (identity_id, guardian_identity_id),
    CHECK (identity_id != guardian_identity_id)
);

CREATE INDEX recovery_guardians_guardian_idx ON recovery_guardians(guardian_identity_id);

CREATE TABLE recovery_requests (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    identity_id UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    -- webauthn-rs's serialized `Passkey` for the new device, captured at
    -- request-completion time — same shape `identity_keys.passkey_data`
    -- stores, but held here, uncommitted, until finalize.
    pending_passkey_data JSONB NOT NULL,
    pending_credential_id BYTEA NOT NULL,
    pending_device_label TEXT,
    threshold_at_request INT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending_approvals'
        CHECK (status IN ('pending_approvals', 'delay', 'completed', 'cancelled')),
    requested_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    delay_ends_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    cancelled_at TIMESTAMPTZ,
    cancelled_by UUID REFERENCES identities(id),
    cancel_reason TEXT
);

-- At most one *active* (not completed/cancelled) recovery request per
-- identity at a time — the abuse-resistance/rate-limit backstop the
-- unauthenticated initiation endpoint needs: a second initiation attempt
-- while one is already outstanding is rejected outright rather than
-- silently piling up parallel attempts, and it also means "is a recovery
-- in progress for identity X" is always at most one row to check.
CREATE UNIQUE INDEX recovery_requests_one_active_per_identity
    ON recovery_requests(identity_id)
    WHERE status IN ('pending_approvals', 'delay');

CREATE INDEX recovery_requests_identity_idx ON recovery_requests(identity_id);

CREATE TABLE recovery_approvals (
    request_id UUID NOT NULL REFERENCES recovery_requests(id) ON DELETE CASCADE,
    guardian_identity_id UUID NOT NULL REFERENCES identities(id),
    approved_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (request_id, guardian_identity_id)
);

-- A distinct ceremony kind, same reasoning migration 0022 gives for
-- `add_passkey`: the recovery-initiation ceremony must never be feedable
-- into `handlers::register_finish` (identity creation) or vice versa, kept
-- apart by construction rather than convention.
ALTER TABLE webauthn_ceremonies DROP CONSTRAINT webauthn_ceremonies_kind_check;
ALTER TABLE webauthn_ceremonies
    ADD CONSTRAINT webauthn_ceremonies_kind_check
    CHECK (kind IN ('registration', 'authentication', 'add_passkey', 'recovery_start'));
