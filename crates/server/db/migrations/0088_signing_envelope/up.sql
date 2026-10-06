-- Every signed or hashed layout now carries a header (layout version, rules version), names its
-- hash algorithm and ends with an extensions region (#1362). Stored rows record those values so a
-- verifier never guesses which layout produced a hash. The layouts changed with no compatibility:
-- a database holding entries, tree heads or cosignatures must be wiped, not migrated.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM ledger_entries)
        OR EXISTS (SELECT 1 FROM mirrored_entries)
        OR EXISTS (SELECT 1 FROM signed_tree_heads)
        OR EXISTS (SELECT 1 FROM observed_sths)
        OR EXISTS (SELECT 1 FROM witness_cosignatures)
        OR EXISTS (SELECT 1 FROM equivocation_evidence)
        OR EXISTS (SELECT 1 FROM network_migration_checkpoints WHERE source_signature IS NOT NULL)
    THEN
        RAISE EXCEPTION 'migration 0088 adds the signing envelope to every layout (#1362); data written before it cannot verify, so wipe this database (make db-reset) before migrating';
    END IF;
END $$;

ALTER TABLE ledger_entries
    ADD COLUMN layout_version SMALLINT NOT NULL CHECK (layout_version >= 1),
    ADD COLUMN rules_version INTEGER NOT NULL CHECK (rules_version >= 1),
    ADD COLUMN hash_algo SMALLINT NOT NULL CHECK (hash_algo BETWEEN 1 AND 255),
    ADD COLUMN extensions BYTEA NOT NULL;

ALTER TABLE mirrored_entries
    ADD COLUMN layout_version SMALLINT NOT NULL CHECK (layout_version >= 1),
    ADD COLUMN rules_version INTEGER NOT NULL CHECK (rules_version >= 1),
    ADD COLUMN hash_algo SMALLINT NOT NULL CHECK (hash_algo BETWEEN 1 AND 255),
    ADD COLUMN extensions BYTEA NOT NULL;

ALTER TABLE signed_tree_heads
    ADD COLUMN layout_version SMALLINT NOT NULL CHECK (layout_version >= 1),
    ADD COLUMN rules_version INTEGER NOT NULL CHECK (rules_version >= 1),
    ADD COLUMN hash_algo SMALLINT NOT NULL CHECK (hash_algo BETWEEN 1 AND 255),
    ADD COLUMN extensions BYTEA NOT NULL;

ALTER TABLE observed_sths
    ADD COLUMN layout_version SMALLINT NOT NULL CHECK (layout_version >= 1),
    ADD COLUMN rules_version INTEGER NOT NULL CHECK (rules_version >= 1),
    ADD COLUMN hash_algo SMALLINT NOT NULL CHECK (hash_algo BETWEEN 1 AND 255),
    ADD COLUMN extensions BYTEA NOT NULL;

ALTER TABLE witness_cosignatures
    ADD COLUMN layout_version SMALLINT NOT NULL CHECK (layout_version >= 1),
    ADD COLUMN rules_version INTEGER NOT NULL CHECK (rules_version >= 1),
    ADD COLUMN hash_algo SMALLINT NOT NULL CHECK (hash_algo BETWEEN 1 AND 255),
    ADD COLUMN extensions BYTEA NOT NULL;

-- The two conflicting heads of a piece of evidence each keep their envelope as JSON (the
-- `EnvelopeWire` shape); the cosignatures already stored as JSON carry their own.
ALTER TABLE equivocation_evidence
    ADD COLUMN envelope_a JSONB NOT NULL,
    ADD COLUMN envelope_b JSONB NOT NULL;

-- The source head recorded at a migration checkpoint, present exactly when its signature is.
ALTER TABLE network_migration_checkpoints
    ADD COLUMN source_layout_version SMALLINT,
    ADD COLUMN source_rules_version INTEGER,
    ADD COLUMN source_hash_algo SMALLINT,
    ADD COLUMN source_extensions BYTEA,
    ADD CONSTRAINT network_migration_checkpoints_source_envelope CHECK (
        (source_signature IS NULL) = (source_layout_version IS NULL)
        AND (source_signature IS NULL) = (source_rules_version IS NULL)
        AND (source_signature IS NULL) = (source_hash_algo IS NULL)
        AND (source_signature IS NULL) = (source_extensions IS NULL)
    );
