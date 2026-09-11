-- Attestation issuance (issue #32) — signed proof that an issuer claims a
-- subject earned an achievement/milestone. A rebuildable projection of
-- `achievement.issued`/`milestone.issued` (`crates/server/src/achievements.rs`),
-- same posture as every other projection in this repo: the outbox-written
-- event is canonical, this row is a cache of it.
--
-- No `revoked_at` column — #81 decided revocation is its own append-only
-- entry, not a mutable status field on the attestation (#75's ADR forbids
-- that shape for durable history); that's #85, not built here. The
-- `indexer_attestations` projection (migration 0015) already has its own
-- `revoked_at` for exactly this reason — it's a read-model *cache* of
-- whatever #85 eventually emits, which is a different thing from this
-- table mutating its own row.
--
-- `id` is the attestation's own id, not derived from anything — unlike a
-- definition's id, two issuances of the same achievement to the same
-- subject are two distinct, equally real attestations (a repeatable
-- achievement is issued more than once on purpose).
CREATE TABLE achievement_attestations (
    id UUID PRIMARY KEY,
    game_id UUID NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    issuer TEXT NOT NULL,
    subject UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    achievement TEXT NOT NULL,
    issued_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- The Signature that authorized this attestation (crate::games'
    -- IssuerKey scheme, #80/#84) — stored for audit/re-verification, not
    -- re-checked on every read.
    proof_key_id UUID NOT NULL,
    proof_algorithm TEXT NOT NULL,
    proof_bytes BYTEA NOT NULL
);

CREATE INDEX achievement_attestations_subject_idx ON achievement_attestations(subject);
CREATE INDEX achievement_attestations_game_id_idx ON achievement_attestations(game_id);
