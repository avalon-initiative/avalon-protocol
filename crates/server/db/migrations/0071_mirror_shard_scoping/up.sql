-- Issue #604: mirror storage/verification (observed_sths, mirrored_entries,
-- equivocation_findings) predates sharding (#527/#532) and has always been
-- keyed on network_id alone -- but every shard under a network_id has its
-- own independent log with its own independent seq/tree_size numbering.
-- A node mirroring more than one shard of the same network (now a common
-- case since #599's auto-discovery, previously only reachable via manual
-- multi-entry AVALON_MIRROR_PEERS config) had its inclusion-proof
-- verification state silently collide between shards, and two unrelated
-- shards both legitimately reaching the same tree_size (e.g. tree_size=1,
-- which every new shard passes through) could even be misreported as a
-- false equivocation. #573 (closed) already fixed the equivalent gap on
-- the read/serving side, scoped by source_url as a proxy for shard
-- identity -- an imperfect proxy, since one peer can serve more than one
-- shard (this project's own avalon-peer sandbox node does exactly that).
-- This migration adds the real, direct shard_id dimension throughout.
--
-- Defaults to 'core' everywhere, matching AVALON_OWN_SHARD_ID's own
-- default and every pre-#527 deployment's implicit single shard -- every
-- existing row un-ambiguously belonged to the (at the time, only) core
-- shard, so backfilling that value is exactly correct, not a guess.

ALTER TABLE observed_sths ADD COLUMN shard_id TEXT NOT NULL DEFAULT 'core';
ALTER TABLE observed_sths DROP CONSTRAINT observed_sths_source_url_network_id_tree_size_key;
ALTER TABLE observed_sths ADD CONSTRAINT observed_sths_source_url_network_id_shard_id_tree_size_key
    UNIQUE (source_url, network_id, shard_id, tree_size);
DROP INDEX observed_sths_network_tree_idx;
CREATE INDEX observed_sths_network_shard_tree_idx ON observed_sths(network_id, shard_id, tree_size);

ALTER TABLE mirrored_entries ADD COLUMN shard_id TEXT NOT NULL DEFAULT 'core';
ALTER TABLE mirrored_entries DROP CONSTRAINT mirrored_entries_network_id_seq_key;
ALTER TABLE mirrored_entries ADD CONSTRAINT mirrored_entries_network_id_shard_id_seq_key
    UNIQUE (network_id, shard_id, seq);

ALTER TABLE equivocation_findings ADD COLUMN shard_id TEXT NOT NULL DEFAULT 'core';
DROP INDEX equivocation_findings_network_tree_idx;
CREATE INDEX equivocation_findings_network_shard_tree_idx
    ON equivocation_findings(network_id, shard_id, tree_size);
