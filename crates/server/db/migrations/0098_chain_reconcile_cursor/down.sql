DROP INDEX identity_chain_state_anchor_dependent_idx;
CREATE INDEX identity_chain_state_anchor_dependent_idx ON identity_chain_state (identity_id)
    WHERE anchor_dependent;
ALTER TABLE identity_chain_state DROP COLUMN last_reconciled_at;
