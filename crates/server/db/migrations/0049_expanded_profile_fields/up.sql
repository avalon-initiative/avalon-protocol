-- Expanded self-described profile fields (issue #372), same shape/conventions
-- as #155's bio/favorite_genres/pronouns.
--
-- `links` is stored as `TEXT[]`, matching how `favorite_genres` is stored
-- (see 0018_profile_self_description/up.sql) — server-side validation
-- (`crates/server/src/handlers.rs`) enforces the count/length/URL caps, not
-- the column type.
--
-- `location` is self-described free text only — never IP-derived or
-- geocoded (see docs/architecture/identity.md and
-- crates/protocol/src/identity.rs's `Profile::location` doc comment).
ALTER TABLE profiles ADD COLUMN banner_url TEXT;
ALTER TABLE profiles ADD COLUMN status TEXT;
ALTER TABLE profiles ADD COLUMN links TEXT[] NOT NULL DEFAULT '{}';
ALTER TABLE profiles ADD COLUMN timezone TEXT;
ALTER TABLE profiles ADD COLUMN theme_color TEXT;
ALTER TABLE profiles ADD COLUMN location TEXT;
