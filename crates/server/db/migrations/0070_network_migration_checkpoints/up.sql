-- Issue #484 (per #476/#479's decision): the audit trail a deliberate
-- mainnet-N -> mainnet-(N+1) genesis reset leaves behind on the *new*
-- network's own database. `avalon_chain::migration::migrate_network`
-- writes exactly one row here per source network it cuts over from,
-- recording that network's final Signed Tree Head (or, if the source had
-- a genesis but never actually committed anything, a zero-tree_size
-- checkpoint with no STH fields to reference) as the point this network's
-- history picks up from. `source_root_hash`/`source_signing_key_id`/
-- `source_signature`/`source_sth_created_at` are nullable for exactly that
-- zero-entry case; `source_tree_size` is not, since it's always known (0
-- or otherwise) even when no STH row exists yet.
--
-- Deliberately not a singleton like `chain_genesis` (migration
-- 0016_chain_genesis) — a network could in principle be the target of a
-- migration more than once over its life, and this table is a append-only
-- history of that, not a single fixed fact about the database.
CREATE TABLE network_migration_checkpoints (
    id UUID PRIMARY KEY,
    source_network_id TEXT NOT NULL,
    source_tree_size BIGINT NOT NULL,
    source_root_hash TEXT,
    source_signing_key_id TEXT,
    source_signature TEXT,
    source_sth_created_at TIMESTAMPTZ,
    migrated_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX network_migration_checkpoints_source_idx
    ON network_migration_checkpoints (source_network_id, source_tree_size);
