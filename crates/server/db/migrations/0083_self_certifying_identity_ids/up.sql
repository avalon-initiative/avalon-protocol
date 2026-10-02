-- Identity ids become self-certifying: lowercase hex SHA-256 of a domain tag and the inception
-- public key, stored as TEXT with a shape CHECK. No backward compatibility: every identity-keyed
-- table is truncated (dev databases are wiped; run `make db-reset` for a clean ledger too).
-- The column set is discovered from the catalog (every FK to identities(id)) plus the projection
-- and replica tables that carry identity ids without an FK.
SET LOCAL lock_timeout = '10s';

CREATE TEMP TABLE identity_fk_cols ON COMMIT DROP AS
SELECT c.conrelid::regclass::text AS tbl,
       c.conname AS conname,
       a.attname AS col,
       pg_get_constraintdef(c.oid) AS def
FROM pg_constraint c
JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = c.conkey[1]
WHERE c.contype = 'f'
  AND c.confrelid = 'identities'::regclass
  AND array_length(c.conkey, 1) = 1;

CREATE TEMP TABLE identity_plain_cols (tbl text, col text) ON COMMIT DROP;
INSERT INTO identity_plain_cols VALUES
    ('guild_messages_replica', 'author'),
    ('conversation_messages_replica', 'author'),
    ('indexer_game_bindings', 'identity_id'),
    ('indexer_game_data_instances', 'subject'),
    ('identity_chain_events', 'identity_id'),
    ('identity_chain_state', 'identity_id');

TRUNCATE identities CASCADE;
TRUNCATE guild_messages_replica, conversation_messages_replica, indexer_game_bindings,
         indexer_game_data_instances, identity_chain_events, identity_chain_state;

DO $$
DECLARE
    r record;
BEGIN
    FOR r IN SELECT * FROM identity_fk_cols LOOP
        EXECUTE format('ALTER TABLE %s DROP CONSTRAINT %I', r.tbl, r.conname);
    END LOOP;

    ALTER TABLE identities ALTER COLUMN id TYPE TEXT USING id::text;
    ALTER TABLE identities ADD COLUMN inception_public_key BYTEA NOT NULL;
    ALTER TABLE identities ADD CONSTRAINT identities_id_self_certifying CHECK (
        id = encode(sha256('avalon-identity-id-v1'::bytea || inception_public_key), 'hex')
    );

    FOR r IN SELECT tbl, col FROM identity_fk_cols UNION SELECT tbl, col FROM identity_plain_cols LOOP
        EXECUTE format('ALTER TABLE %s ALTER COLUMN %I TYPE TEXT USING %I::text', r.tbl, r.col, r.col);
        EXECUTE format(
            'ALTER TABLE %s ADD CONSTRAINT %I CHECK (%I ~ ''^[0-9a-f]{64}$'')',
            r.tbl, r.tbl || '_' || r.col || '_hex_chk', r.col
        );
    END LOOP;

    FOR r IN SELECT * FROM identity_fk_cols LOOP
        EXECUTE format('ALTER TABLE %s ADD CONSTRAINT %I %s', r.tbl, r.conname, r.def);
    END LOOP;
END $$;
