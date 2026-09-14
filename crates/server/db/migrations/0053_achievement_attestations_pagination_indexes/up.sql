-- Issue #377: GET /me/achievements gains cursor pagination and
-- integrator/claim_kind filters. `achievement_attestations` already has a
-- plain `subject` index (0045) and an `integrator_id` index (0045, renamed
-- 0052) but nothing composite, so the new `ORDER BY issued_at DESC, id
-- DESC` cursor and the new `integrator_id` filter would otherwise fall
-- back to a full per-subject scan + sort.
CREATE INDEX achievement_attestations_subject_issued_at_idx
    ON achievement_attestations (subject, issued_at DESC, id DESC);

CREATE INDEX achievement_attestations_subject_integrator_idx
    ON achievement_attestations (subject, integrator_id);
