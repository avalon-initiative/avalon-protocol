DELETE FROM indexer_applied_events a USING indexer_applied_events b
WHERE a.event_id = b.event_id AND a.ctid > b.ctid;
ALTER TABLE indexer_applied_events DROP CONSTRAINT indexer_applied_events_pkey;
ALTER TABLE indexer_applied_events DROP COLUMN shard_id;
ALTER TABLE indexer_applied_events ADD PRIMARY KEY (event_id);
DROP TABLE indexer_identity_homes;
ALTER TABLE mirrored_entries DROP COLUMN projection_rejection;
DROP INDEX indexer_identity_signing_keys_identity_public_key_idx;
