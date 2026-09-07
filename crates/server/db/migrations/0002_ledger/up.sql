-- Hash-chained, append-only ledger of durable protocol events. Owned by
-- avalon-chain's domain logic, but this migration lives here (not under
-- crates/chain/db/) because milestone 1 has exactly one shared Postgres
-- database — see crates/chain/src/postgres.rs. Split it out if/when chain
-- ever gets its own independent deployment.
--
-- No cryptographic signature yet (that's issue #39 — signing scheme and key
-- management, still open). `entry_hash` gives tamper-evidence via hash
-- chaining today; signing an entry is a stronger, separate guarantee to add
-- later without changing this shape.
CREATE TABLE ledger_entries (
    seq BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    event_id UUID NOT NULL,
    kind TEXT NOT NULL,
    issuer TEXT NOT NULL,
    subject TEXT NOT NULL,
    payload JSONB NOT NULL,
    event_timestamp TIMESTAMPTZ NOT NULL,
    version INT NOT NULL,
    prev_hash TEXT NOT NULL,
    entry_hash TEXT NOT NULL UNIQUE,
    committed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX ledger_entries_subject_idx ON ledger_entries(subject);
