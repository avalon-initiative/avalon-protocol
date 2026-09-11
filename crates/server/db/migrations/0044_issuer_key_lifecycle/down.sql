DROP INDEX IF EXISTS issuer_keys_game_id_active_idx;

ALTER TABLE issuer_keys
    DROP COLUMN IF EXISTS role,
    DROP COLUMN IF EXISTS valid_until,
    DROP COLUMN IF EXISTS revoked_at,
    DROP COLUMN IF EXISTS revoked_reason;
