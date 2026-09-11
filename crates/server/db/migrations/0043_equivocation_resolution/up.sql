-- Mirror recovery (issue #316, implementing #300's decided POC scope for
-- equivocation response): 0041_mirror_observations built detection
-- (equivocation_findings) but gave a mirror no way to record that a
-- finding was investigated and resolved, or to recover from it — the
-- mirror-watcher's equivocation gate (crates/server/src/mirror_watcher.rs)
-- just refuses to backfill a network forever once any finding exists for
-- it, with no way to clear that state short of a manual DB edit.
--
-- resolved_at / resolved_root_hash record a human decision: after
-- investigation (per docs/maintainers/equivocation-response.md), an
-- operator determines which of the two disagreeing root_hash values was
-- the legitimate tree and marks the finding resolved with that hash. Both
-- columns are set together or not at all — there is no partial-resolution
-- state.
ALTER TABLE equivocation_findings
    ADD COLUMN resolved_at TIMESTAMPTZ,
    ADD COLUMN resolved_root_hash TEXT;

-- Lets the mirror-watcher's equivocation gate cheaply check "does this
-- network have any *unresolved* finding" without scanning every row.
CREATE INDEX equivocation_findings_network_unresolved_idx
    ON equivocation_findings(network_id)
    WHERE resolved_at IS NULL;
