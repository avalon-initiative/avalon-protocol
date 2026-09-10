-- `indexer_attestations` (0015_indexer_projections) was only indexed on
-- `subject`. The Game Registry achievement metrics added in #261 --
-- `crates/indexer/src/projections/attestations.rs::issued_count`/
-- `revoked_count`/`unique_holder_count` -- all filter `WHERE issuer = $1`,
-- so every one of those reads forced a sequential scan with no index on
-- `issuer` at all.
--
-- `(issuer, revoked_at)` covers `issued_count` (equality on the leading
-- column) and `revoked_count` (`issuer = $1 AND revoked_at IS NOT NULL`)
-- directly. `(issuer, subject)` separately covers `unique_holder_count`'s
-- `COUNT(DISTINCT subject)` over `issuer = $1` -- a composite ending in
-- `revoked_at` wouldn't help that scan, since the distinct is over
-- `subject`, not `revoked_at`.
CREATE INDEX indexer_attestations_issuer_revoked_at_idx
    ON indexer_attestations (issuer, revoked_at);

CREATE INDEX indexer_attestations_issuer_subject_idx
    ON indexer_attestations (issuer, subject);
