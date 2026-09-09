-- Singleton table committing the ledger to a network identity (issue #173).
-- `id` is always `true` and is the primary key, so Postgres itself enforces
-- at most one row — the standard boolean-singleton trick, since Postgres has
-- no native "exactly one row" constraint. `network_id` is written once, at
-- genesis, and never updated by any code path: see
-- `PostgresSettlementProvider::connect` in `crates/chain/src/postgres.rs`.
CREATE TABLE chain_genesis (
    id BOOLEAN PRIMARY KEY DEFAULT true CHECK (id),
    network_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
