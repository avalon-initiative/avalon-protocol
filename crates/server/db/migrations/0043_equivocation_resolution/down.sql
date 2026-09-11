DROP INDEX IF EXISTS equivocation_findings_network_unresolved_idx;

ALTER TABLE equivocation_findings
    DROP COLUMN IF EXISTS resolved_at,
    DROP COLUMN IF EXISTS resolved_root_hash;
