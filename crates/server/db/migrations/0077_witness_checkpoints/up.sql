-- This node's own record of the last tree head it has itself cosigned as a
-- witness, per (network_id, shard_id) — the consistency-proof-extension
-- check a real cosigning decision needs before ever cosigning again for the
-- same log (avalon_protocol::witness/cosigned_sth's design). One row per
-- log: a node has exactly one witness identity, and only ever cosigns each
-- log's heads in strictly increasing tree_size order, so there is exactly
-- one "last cosigned" checkpoint to track per log, not a history.
-- `witness_key_id` is carried for audit/debugging, not identity — it is
-- always this node's own witness key at the time it cosigned.
CREATE TABLE witness_checkpoints (
    network_id TEXT NOT NULL,
    shard_id TEXT NOT NULL,
    tree_size BIGINT NOT NULL,
    root_hash TEXT NOT NULL,
    witness_key_id TEXT NOT NULL,
    cosigned_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (network_id, shard_id)
);
