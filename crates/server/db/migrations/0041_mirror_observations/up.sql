-- Mirror-watcher storage (issue #299, implementing #40's decided
-- no-consensus mirror model — see docs/projects/backend-server/architecture/settlement.md's
-- "Mirror sync stays minimal" section): any mirror that independently
-- observes and stores every Signed Tree Head it sees for a network can be
-- compared against another mirror's (or its own past) observations at the
-- same tree_size; two different root_hash values claiming the same
-- tree_size from the same operator is cryptographic proof of equivocation.
--
-- observed_sths is deliberately separate from signed_tree_heads
-- (0024_signed_tree_heads) — that table only ever holds STHs *this* node
-- itself produced as a Settlement authority (empty on a pure mirror);
-- this one holds every STH this node has fetched and signature-verified
-- from a peer, tagged by which peer it came from. A node that is both an
-- authority and a mirror-watcher (watching itself, or another authority)
-- has rows in both tables.
CREATE TABLE observed_sths (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    source_url TEXT NOT NULL,
    network_id TEXT NOT NULL,
    tree_size BIGINT NOT NULL,
    root_hash TEXT NOT NULL,
    signature TEXT NOT NULL,
    signing_key_id TEXT NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- One observation per (peer, network, tree_size) — re-observing the
    -- same STH from the same peer on a later poll tick is a no-op, not a
    -- new row.
    UNIQUE (source_url, network_id, tree_size)
);

CREATE INDEX observed_sths_network_tree_idx ON observed_sths(network_id, tree_size);

-- A detected equivocation: two different root_hash values for the same
-- network_id + tree_size, from two different sources (a source of
-- "self:signed-history" means this node's own signed_tree_heads, if it is
-- also a Settlement authority). Durable, human/monitoring-visible record —
-- see crates/chain/src/mirror.rs's module doc comment for the full
-- surfacing mechanism (this table plus an error-level log line).
CREATE TABLE equivocation_findings (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    network_id TEXT NOT NULL,
    tree_size BIGINT NOT NULL,
    source_a TEXT NOT NULL,
    root_hash_a TEXT NOT NULL,
    source_b TEXT NOT NULL,
    root_hash_b TEXT NOT NULL,
    detected_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX equivocation_findings_network_tree_idx ON equivocation_findings(network_id, tree_size);

-- Full ledger entry content a mirror has fetched from a peer's bulk
-- entries endpoint (GET /ledger/entries) and independently verified via an
-- RFC 6962 inclusion proof against a signature-checked STH, before ever
-- being written here. Deliberately its own table, not a write into this
-- node's own ledger_entries — a mirror's local copy of a peer's ledger is
-- not the same thing as this node's own settlement history (a node can be
-- a pure mirror with an empty ledger_entries, or an authority mirroring a
-- *different* authority for cross-checking).
--
-- Uniqueness (and therefore backfill progress) is keyed on (network_id,
-- seq), not per peer — issue #299's multi-peer backfill deliberately
-- treats every configured peer of the same network as an interchangeable
-- source of the same independently-verified content: if peer A is
-- unreachable mid-backfill, peer B can supply the rest of the same
-- network's entries without duplicating or restarting progress. `source_url`
-- is kept purely as an audit trail of which peer this specific entry
-- happened to be fetched from, not as part of the identity of the row.
CREATE TABLE mirrored_entries (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    source_url TEXT NOT NULL,
    network_id TEXT NOT NULL,
    seq BIGINT NOT NULL,
    event_id UUID NOT NULL,
    kind TEXT NOT NULL,
    issuer TEXT NOT NULL,
    subject TEXT NOT NULL,
    payload JSONB,
    event_timestamp TIMESTAMPTZ NOT NULL,
    version INT NOT NULL,
    prev_hash TEXT NOT NULL,
    entry_hash TEXT NOT NULL,
    batch_id UUID NOT NULL,
    -- The tree_size of the STH this entry's inclusion was verified against
    -- at the time it was accepted.
    verified_tree_size BIGINT NOT NULL,
    mirrored_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (network_id, seq)
);

CREATE INDEX mirrored_entries_network_seq_idx ON mirrored_entries(network_id, seq);
