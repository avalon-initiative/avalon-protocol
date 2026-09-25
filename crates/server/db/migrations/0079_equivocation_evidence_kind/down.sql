DELETE FROM equivocation_evidence WHERE evidence_kind = 'author';
ALTER TABLE equivocation_evidence
    DROP CONSTRAINT equivocation_evidence_witness_kind_has_witnesses,
    DROP CONSTRAINT equivocation_evidence_kind_valid,
    DROP COLUMN evidence_kind;
