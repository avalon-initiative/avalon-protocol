-- Public key of each self-certifying (`node:<sha256-of-key>`) shard this node has
-- verified. The id fixes the key, so the key never changes; `last_seen_at` lets an
-- idle pin be evicted and `source_url` bounds how many pins one source may hold.
CREATE TABLE self_certifying_shard_keys (
    shard_id TEXT PRIMARY KEY,
    public_key BYTEA NOT NULL,
    source_url TEXT NOT NULL,
    first_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT self_certifying_shard_keys_id_namespace CHECK (shard_id LIKE 'node:%'),
    CONSTRAINT self_certifying_shard_keys_key_len CHECK (octet_length(public_key) = 32)
);
CREATE INDEX self_certifying_shard_keys_last_seen ON self_certifying_shard_keys (last_seen_at);
