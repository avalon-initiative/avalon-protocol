-- Key event acceptance no longer depends on which shard delivered the event (#1409): the per
-- identity "home shard" record is gone. The shard stays only as unsigned metadata on the record.
DROP TABLE indexer_identity_homes;
