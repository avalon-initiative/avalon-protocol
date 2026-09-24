-- Achievement/milestone icons (issue #332): a definition gains a visual
-- identity beyond name/description/key. Both columns are optional — the
-- server falls back to a single hardcoded default icon key
-- (crates/server/src/achievements.rs's DEFAULT_ICON) at read time when
-- neither is set, so no backfill is needed here for existing rows.
--
-- `icon` is a key into a small, fixed built-in icon set shipped with
-- the shared UI library (trophy/star/shield/sword — generic enough to cover
-- games/apps/services alike). `icon_url` is an integrator-hosted image,
-- taking precedence over `icon` when present — stored as-is, never
-- fetched/validated server-side beyond a basic http(s)-scheme check, same
-- "the server records, it doesn't vouch for content" posture `avatar_url`
-- already has in this schema.
ALTER TABLE achievement_definitions
    ADD COLUMN icon TEXT,
    ADD COLUMN icon_url TEXT;
