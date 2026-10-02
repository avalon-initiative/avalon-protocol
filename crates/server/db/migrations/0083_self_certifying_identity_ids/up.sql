-- Identity ids become self-certifying: lowercase hex SHA-256 of a domain tag and the inception
-- public key, stored as TEXT with a shape CHECK. No backward compatibility: every identity-keyed
-- table (and webauthn_ceremonies) is truncated; the ledger and outbox are not. Dev databases are wiped: run `make db-reset` for a clean ledger too.
-- The column set is discovered from the catalog (every FK to identities(id)) plus the projection
-- and replica tables that carry identity ids without an FK.
SET LOCAL lock_timeout = '10s';

-- Refuses to run over existing data: this migration deletes identities and identity-keyed rows.
DO $guard$
DECLARE
    t text;
    found boolean;
BEGIN
    IF coalesce(current_setting('avalon.allow_identity_wipe', true), '') = 'on' THEN
        RETURN;
    END IF;
    FOREACH t IN ARRAY ARRAY[
        'identities', 'webauthn_ceremonies', 'guild_messages_replica',
        'conversation_messages_replica', 'indexer_integrator_bindings',
        'indexer_integrator_data_instances', 'identity_chain_events', 'identity_chain_state'
    ] LOOP
        EXECUTE format('SELECT EXISTS (SELECT 1 FROM %I)', t) INTO found;
        IF found THEN
            RAISE EXCEPTION 'migration 0083 deletes every identity and all identity-keyed rows (% is not empty; the ledger and outbox are not touched). On a dev database run `make db-reset`. On any other database export or back it up first, then re-run with PGOPTIONS="-c avalon.allow_identity_wipe=on".', t;
        END IF;
    END LOOP;
END
$guard$;


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
    ('indexer_integrator_bindings', 'identity_id'),
    ('indexer_integrator_data_instances', 'subject'),
    ('identity_chain_events', 'identity_id'),
    ('identity_chain_state', 'identity_id');

CREATE TEMP TABLE identity_check_defs ON COMMIT DROP AS
SELECT DISTINCT c.conrelid::regclass::text AS tbl, c.conname AS conname,
       pg_get_constraintdef(c.oid) AS def
FROM pg_constraint c
JOIN (SELECT tbl, col FROM identity_fk_cols UNION SELECT tbl, col FROM identity_plain_cols) ic
  ON ic.tbl = c.conrelid::regclass::text
JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attname = ic.col
WHERE c.contype = 'c' AND a.attnum = ANY (c.conkey) AND c.conname NOT LIKE '%\_hex\_chk';

TRUNCATE identities CASCADE;
TRUNCATE guild_messages_replica, conversation_messages_replica, indexer_integrator_bindings,
         indexer_integrator_data_instances, identity_chain_events, identity_chain_state,
         webauthn_ceremonies;

DO $$
DECLARE
    r record;
BEGIN
    FOR r IN SELECT * FROM identity_fk_cols LOOP
        EXECUTE format('ALTER TABLE %s DROP CONSTRAINT %I', r.tbl, r.conname);
    END LOOP;
    FOR r IN SELECT * FROM identity_check_defs LOOP
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

    FOR r IN SELECT * FROM identity_check_defs LOOP
        EXECUTE format('ALTER TABLE %s ADD CONSTRAINT %I %s', r.tbl, r.conname, r.def);
    END LOOP;

    -- A key id is only meaningful per identity, so another identity cannot occupy a victim's key id.
    ALTER TABLE indexer_identity_signing_keys DROP CONSTRAINT indexer_identity_signing_keys_pkey;
    ALTER TABLE indexer_identity_signing_keys ADD PRIMARY KEY (identity_id, signing_key_id);
    -- Revocations are recorded even when they arrive before the key's addition.
    CREATE TABLE indexer_identity_signing_key_revocations (
        identity_id TEXT NOT NULL CHECK (identity_id ~ '^[0-9a-f]{64}$'),
        signing_key_id UUID NOT NULL,
        revoked_at TIMESTAMPTZ NOT NULL,
        PRIMARY KEY (identity_id, signing_key_id)
    );
    CREATE TABLE indexer_identity_passkey_revocations (
        identity_id TEXT NOT NULL CHECK (identity_id ~ '^[0-9a-f]{64}$'),
        passkey_id UUID NOT NULL,
        revoked_at TIMESTAMPTZ NOT NULL,
        PRIMARY KEY (identity_id, passkey_id)
    );

    FOR r IN SELECT * FROM identity_fk_cols LOOP
        EXECUTE format('ALTER TABLE %s ADD CONSTRAINT %I %s', r.tbl, r.conname, r.def);
    END LOOP;
END $$;
