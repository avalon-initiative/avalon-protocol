-- Issuer key lifecycle (issue #80, decided; implemented here per #84): a
-- two-tier root/operational key role, on top of the `issuer_keys` table
-- migration 0011 already shaped "for #84 to extend rather than replace".
--
-- role: 'root' or 'operational' (see `avalon_protocol::games::KeyRole`).
-- Registration's initial key is always 'root' — it doubles as the issuer's
-- first operational key too (any non-revoked, non-expired key may sign
-- attestations regardless of role; role only gates who may change the key
-- set — see `avalon_protocol::games::IssuerKey::authorizes_key_changes`).
-- Existing rows (every key inserted before this migration) default to
-- 'root', matching what registration has always done.
ALTER TABLE issuer_keys
    ADD COLUMN role TEXT NOT NULL DEFAULT 'root',
    ADD COLUMN valid_until TIMESTAMPTZ,
    ADD COLUMN revoked_at TIMESTAMPTZ,
    ADD COLUMN revoked_reason TEXT;

-- `authenticate_game`'s per-request key lookup filters on non-revoked
-- (and, once past valid_until, non-expired) — this is what makes that
-- lookup cheap without a full table scan as an issuer accumulates
-- historical/revoked keys over time.
CREATE INDEX issuer_keys_game_id_active_idx
    ON issuer_keys(game_id)
    WHERE revoked_at IS NULL;
