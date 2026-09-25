-- Supersedes 0074_equivocation_evidence's table of the same name with a
-- fuller schema (both heads' complete signed fields, not just the shared
-- author key, so a row is reconstructable and re-verifiable from itself
-- alone with no dependency on any other table still holding the same
-- rows later). 0074 landed no real rows yet (its writer is being
-- switched to this table in the same change), so this drops and recreates
-- rather than carrying an ALTER migration for a table with nothing to
-- preserve.
DROP TABLE IF EXISTS equivocation_evidence;

-- Durable, independently-verifiable evidence that two disjoint
-- witness majorities cosigned two different tree heads at the same
-- network_id/shard_id/tree_size — the storage half of
-- avalon_protocol::cosigned_sth::find_equivocating_witnesses. Everything
-- needed to reconstruct the proof from signatures alone is stored
-- directly (both full heads' signed fields, both cosignature sets, and
-- the witness id(s) the two sets share) rather than a pointer to rows
-- that could later be pruned — this row must remain independently
-- verifiable on its own indefinitely. Root hashes are stored in a fixed
-- lexical order (root_hash_a < root_hash_b) so the same conflicting pair
-- is never recorded twice under swapped labels.
CREATE TABLE equivocation_evidence (
    network_id TEXT NOT NULL,
    shard_id TEXT NOT NULL,
    tree_size BIGINT NOT NULL,
    root_hash_a TEXT NOT NULL,
    signing_key_id_a TEXT NOT NULL,
    signature_a TEXT NOT NULL,
    author_created_at_a TIMESTAMPTZ NOT NULL,
    cosignatures_a JSONB NOT NULL,
    root_hash_b TEXT NOT NULL,
    signing_key_id_b TEXT NOT NULL,
    signature_b TEXT NOT NULL,
    author_created_at_b TIMESTAMPTZ NOT NULL,
    cosignatures_b JSONB NOT NULL,
    equivocating_witness_key_ids TEXT[] NOT NULL,
    detected_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (network_id, shard_id, tree_size, root_hash_a, root_hash_b),
    CONSTRAINT equivocation_evidence_root_hash_order CHECK (root_hash_a < root_hash_b)
);
