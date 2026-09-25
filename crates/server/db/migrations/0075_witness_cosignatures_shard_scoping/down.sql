ALTER TABLE witness_cosignatures DROP CONSTRAINT witness_cosignatures_pkey;
ALTER TABLE witness_cosignatures ADD PRIMARY KEY (network_id, tree_size, witness_key_id);
ALTER TABLE witness_cosignatures DROP COLUMN shard_id;
