-- Witness cosignature storage (#932, wiring the #934 design-spike
-- primitives in avalon_protocol::witness/cosigned_sth into the server).
-- One row per (network_id, tree_size, witness_key_id): a witness may
-- cosign a given tree head at most once, matching `WitnessCosignature`'s
-- own no-double-cosign rule. `tree_size`/`root_hash`/`network_id`/
-- `author_created_at` mirror the exact STH this cosignature is bound to
-- (`signed_tree_heads`, no foreign key — an STH's own primary key is just
-- `tree_size`, and a cosignature is meaningful even for an STH this node
-- hasn't stored yet, e.g. one relayed by gossip ahead of the author's own
-- batch commit reaching this node). `witness_key_id` is not a foreign key
-- either — the known list a verifier trusts a given key against is
-- client-side state (`avalon_protocol::known_list`), not a server-side
-- registry.
CREATE TABLE witness_cosignatures (
    network_id TEXT NOT NULL,
    tree_size BIGINT NOT NULL,
    witness_key_id TEXT NOT NULL,
    root_hash TEXT NOT NULL,
    author_created_at TIMESTAMPTZ NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL,
    signature TEXT NOT NULL,
    PRIMARY KEY (network_id, tree_size, witness_key_id)
);
