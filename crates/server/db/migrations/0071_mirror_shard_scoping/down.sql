DROP INDEX equivocation_findings_network_shard_tree_idx;
CREATE INDEX equivocation_findings_network_tree_idx ON equivocation_findings(network_id, tree_size);
ALTER TABLE equivocation_findings DROP COLUMN shard_id;

ALTER TABLE mirrored_entries DROP CONSTRAINT mirrored_entries_network_id_shard_id_seq_key;
ALTER TABLE mirrored_entries ADD CONSTRAINT mirrored_entries_network_id_seq_key
    UNIQUE (network_id, seq);
ALTER TABLE mirrored_entries DROP COLUMN shard_id;

DROP INDEX observed_sths_network_shard_tree_idx;
CREATE INDEX observed_sths_network_tree_idx ON observed_sths(network_id, tree_size);
ALTER TABLE observed_sths DROP CONSTRAINT observed_sths_source_url_network_id_shard_id_tree_size_key;
ALTER TABLE observed_sths ADD CONSTRAINT observed_sths_source_url_network_id_tree_size_key
    UNIQUE (source_url, network_id, tree_size);
ALTER TABLE observed_sths DROP COLUMN shard_id;
