-- 'witness' rows carry a non-empty equivocating_witness_key_ids (a witness
-- cosigned both roots); 'author' rows are direct proof that the author key
-- signed two roots at one tree_size and may have an empty witness list.
ALTER TABLE equivocation_evidence
    ADD COLUMN evidence_kind TEXT NOT NULL DEFAULT 'witness',
    ADD CONSTRAINT equivocation_evidence_kind_valid CHECK (evidence_kind IN ('author', 'witness')),
    ADD CONSTRAINT equivocation_evidence_witness_kind_has_witnesses CHECK (
        evidence_kind = 'author' OR cardinality(equivocating_witness_key_ids) > 0
    );
