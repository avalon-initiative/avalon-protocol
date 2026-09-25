-- Durable equivocation evidence, gossip-driven fork detection: one
-- row per confirmed instance of two conflicting cosigned tree heads at the
-- same (network_id, shard_id, tree_size) sharing a witness. This is a
-- minimal, reasonable storage shape chosen because no equivocation-evidence
-- table existed in the schema yet; a separately-landed mirror-sync change
-- adding its own may need reconciling with this table.
--
-- Every column needed to re-verify the proof is stored raw (no foreign
-- keys to `signed_tree_heads`/`witness_cosignatures` — the whole point of
-- this evidence is that it must remain independently verifiable from
-- signatures alone, even if the underlying rows it was assembled from are
-- later pruned or never held by this node at all).
CREATE TABLE equivocation_evidence (
    network_id TEXT NOT NULL,
    shard_id TEXT NOT NULL,
    tree_size BIGINT NOT NULL,
    root_hash_a TEXT NOT NULL,
    root_hash_b TEXT NOT NULL,
    author_verify_key TEXT NOT NULL,
    -- JSON array of `witness::WitnessCosignature` for each conflicting head.
    cosignatures_a JSONB NOT NULL,
    cosignatures_b JSONB NOT NULL,
    -- Witness key ids proven to have cosigned both heads.
    equivocating_witness_key_ids JSONB NOT NULL,
    detected_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (network_id, shard_id, tree_size, root_hash_a, root_hash_b)
);
