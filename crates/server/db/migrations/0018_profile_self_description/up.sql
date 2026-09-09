-- Small, player-optional self-description fields (issue #155), same
-- promised-durable tier as `display_name`/`avatar_url` — see #86 and
-- docs/architecture/identity.md's durable-field table.
--
-- `favorite_genres` is a fixed, small controlled vocabulary
-- (`avalon_protocol::identity::Genre`), not free text, stored as `TEXT[]`
-- rather than a normalized join table — matching how `GuildPermission`
-- values are stored as `TEXT[]` on a guild role, not a lookup table, since
-- the vocabulary is small and closed. Server-side validation
-- (`crates/server/src/handlers.rs`) is what actually enforces the
-- vocabulary and the length caps below; the column types themselves are not
-- the enforcement mechanism.
ALTER TABLE profiles ADD COLUMN bio TEXT;
ALTER TABLE profiles ADD COLUMN favorite_genres TEXT[] NOT NULL DEFAULT '{}';
ALTER TABLE profiles ADD COLUMN pronouns TEXT;
