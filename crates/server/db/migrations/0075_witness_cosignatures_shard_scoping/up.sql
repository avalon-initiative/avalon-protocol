-- witness_cosignatures (0073) was keyed only by (network_id,
-- tree_size, witness_key_id) — every shard under a network_id has its own
-- independent tree_size numbering (see mirrored_entries/observed_sths'
-- own shard_id scoping), so two unrelated shards reaching the
-- same tree_size would collide here, letting one shard's cosignatures
-- silently count toward another's majority check. shard_id defaults to
-- 'core', matching every other shard-scoped table's pre-sharding default.
ALTER TABLE witness_cosignatures ADD COLUMN shard_id TEXT NOT NULL DEFAULT 'core';
ALTER TABLE witness_cosignatures DROP CONSTRAINT witness_cosignatures_pkey;
ALTER TABLE witness_cosignatures ADD PRIMARY KEY (network_id, shard_id, tree_size, witness_key_id);
