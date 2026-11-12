-- When the reconcile worker last visited an identity, so it walks flagged identities round robin and
-- backs off one that keeps failing.
ALTER TABLE identity_chain_state ADD COLUMN last_reconciled_at TIMESTAMPTZ;
DROP INDEX identity_chain_state_anchor_dependent_idx;
CREATE INDEX identity_chain_state_anchor_dependent_idx
    ON identity_chain_state (last_reconciled_at NULLS FIRST, identity_id) WHERE anchor_dependent;
