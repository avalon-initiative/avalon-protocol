-- Subject-scoped selective ledger sync (issue #364, implementing #306's
-- decided abuse-floor piece): GET /ledger/entries?subject=&since_seq=
-- filters and paginates in the same query, so the existing single-column
-- ledger_entries_subject_idx (subject alone) isn't enough to keep that
-- ordered by seq without a separate sort — this composite index lets
-- Postgres satisfy the WHERE subject = ... AND seq > ... ORDER BY seq
-- shape directly from the index.
CREATE INDEX ledger_entries_subject_seq_idx ON ledger_entries(subject, seq);
