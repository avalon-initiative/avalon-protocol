-- Attestation revocation (issue #85, implementing #81's decided
-- mechanics): a revocation is a signed, appended entry — never a mutation
-- of `achievement_attestations`. That table gains no column here; this is
-- a wholly separate append-only table, joined at read time
-- (`crates/server/src/attestations.rs::get_attestation`).
--
-- `UNIQUE (attestation_id)` deliberately caps this at one revocation per
-- attestation for now: reinstatement (a later entry reversing a
-- revocation, per #81's decision) has no protocol event kind yet and is
-- explicitly deferred — see `avalon_protocol::achievements::AttestationStatus`'s
-- own doc comment. Lifting this constraint is what reinstatement support
-- will need to do later, not something to leave loose now on spec.
CREATE TABLE attestation_revocations (
    id UUID PRIMARY KEY,
    attestation_id UUID NOT NULL REFERENCES achievement_attestations(id) ON DELETE CASCADE,
    issuer TEXT NOT NULL,
    reason_code TEXT NOT NULL,
    reason TEXT NOT NULL,
    revoked_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    proof_key_id UUID NOT NULL,
    proof_algorithm TEXT NOT NULL,
    proof_bytes BYTEA NOT NULL,
    UNIQUE (attestation_id)
);
