-- Signed Tree Heads (issue #210, implementing #39/#40's decided design —
-- see docs/architecture/settlement.md's "What is decided (continued)"
-- section): one row per batch commit, `tree_size` equal to that batch's
-- `ledger_batches.last_seq` — the STH-only signing scheme #39 settled on
-- (Certificate Transparency precedent: no per-entry signatures, only the
-- tree head itself is signed). `root_hash` is the RFC 6962 Merkle Tree
-- Hash of the whole ledger's `ledger_entries.entry_hash` values, ordered
-- by `seq`, at that `tree_size` (`crates/chain/src/merkle.rs`) — the same
-- value `ledger_batches.batch_root` now stores for its batch, replacing
-- the placeholder chain-tip root #38 shipped.
--
-- The private signing key never lives in this table, or anywhere in
-- Postgres — it's loaded from the environment at commit time
-- (`AVALON_SETTLEMENT_SIGNING_KEY`, see `.env.example`). `signing_key_id`
-- is only a caller-chosen label distinguishing this settlement-operator
-- key domain from issuer keys (#80/#84) and player keys (#73); it is not a
-- foreign key into anything here.
CREATE TABLE signed_tree_heads (
    tree_size BIGINT PRIMARY KEY,
    root_hash TEXT NOT NULL,
    network_id TEXT NOT NULL,
    signing_key_id TEXT NOT NULL,
    signature TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL
);
