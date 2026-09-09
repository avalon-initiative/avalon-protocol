-- Node-tiered durable history retention (issue #208, implementing #180's
-- decision): settlement commitment vs. durable event storage are separate
-- retention problems. The hash-chained/Merkle-committed log itself
-- (`entry_hash`, `prev_hash`, `seq`, `batch_id`, `ledger_batches`,
-- `signed_tree_heads`) is untouched by this migration and stays permanent
-- on every node regardless of retention tier — only the raw signed event
-- *body* (`payload`) is a candidate for tiered retention.
--
-- `payload` becomes nullable so a hot-tier node can prune old entries'
-- payload without deleting the row itself — `seq`, `entry_hash`,
-- `prev_hash`, `kind`, `issuer`, `subject`, `event_timestamp`, `version`,
-- and `batch_id` all survive pruning, which is exactly what keeps the
-- hash-chain and Merkle-tree structures fully intact and verifiable even
-- after a row's payload is gone (see `crates/chain/src/retention.rs`).
--
-- `payload_pruned_at` records when (if ever) a row's payload was pruned —
-- NULL means the payload is still present. `avalon inspect-ledger` uses
-- this to report whether it's looking at a full or a (partially) pruned
-- hot-tier node, and `avalon-chain::retention`'s pruning query uses it to
-- avoid re-selecting rows it already pruned.
ALTER TABLE ledger_entries ALTER COLUMN payload DROP NOT NULL;
ALTER TABLE ledger_entries ADD COLUMN payload_pruned_at TIMESTAMPTZ;

-- The pruning query filters on `committed_at < cutoff AND payload_pruned_at
-- IS NULL` — a partial index keeps that cheap even after a large fraction
-- of the table has already been pruned (the index shrinks as rows are
-- pruned, since pruned rows no longer match the partial predicate).
CREATE INDEX ledger_entries_prunable_idx ON ledger_entries(committed_at)
    WHERE payload_pruned_at IS NULL;
