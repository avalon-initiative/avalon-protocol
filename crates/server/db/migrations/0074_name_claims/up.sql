-- Verified domain-proven name claims for self-certifying shard ids. One row
-- per name (globally unique); a self-certifying id may hold more than one
-- verified name. Not a foreign-keyed registry entry: `self_certifying_id`
-- and `public_key` verify against each other and against `signature`
-- entirely off this table's contents, so a row here is a cache of an
-- already-proven fact, not the source of trust for it.
CREATE TABLE name_claims (
    name TEXT PRIMARY KEY,
    self_certifying_id TEXT NOT NULL,
    public_key TEXT NOT NULL,
    claim_created_at TIMESTAMPTZ NOT NULL,
    signature TEXT NOT NULL,
    proof_method TEXT NOT NULL,
    verified_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX name_claims_self_certifying_id_idx ON name_claims (self_certifying_id);
