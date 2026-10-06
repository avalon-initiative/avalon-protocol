-- The signed tree head, witness cosignature and witness announce messages moved to the
-- structured signing-bytes layout (#1226) with no compatibility: a stored signature made over the
-- old bytes can no longer verify, so a database holding any must be wiped, not migrated.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM signed_tree_heads)
        OR EXISTS (SELECT 1 FROM observed_sths)
        OR EXISTS (SELECT 1 FROM witness_cosignatures)
        OR EXISTS (SELECT 1 FROM equivocation_evidence)
        OR EXISTS (SELECT 1 FROM network_migration_checkpoints WHERE source_signature IS NOT NULL)
    THEN
        RAISE EXCEPTION 'migration 0087 changes the tree head and witness cosignature signing bytes (#1226); signatures stored before it cannot verify, so wipe this database (make db-reset) before migrating';
    END IF;
END $$;

-- A cosignature now covers the author's key id and signature, so both are stored with it.
ALTER TABLE witness_cosignatures ADD COLUMN author_key_id TEXT NOT NULL;
ALTER TABLE witness_cosignatures ADD COLUMN author_signature TEXT NOT NULL CHECK (length(author_signature) = 128);
