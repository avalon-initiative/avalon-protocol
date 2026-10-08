DROP TABLE indexer_issuer_key_revocations;
DROP TABLE indexer_issuer_keys;
DROP TABLE indexer_issuers;
ALTER TABLE indexer_attestations DROP COLUMN shard_id, DROP COLUMN network_id;
