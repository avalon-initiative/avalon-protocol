-- A guild's roles previously had no uniqueness constraint on name at
-- all — nothing stopped a guild from having several roles all named
-- "master" (or any other collision), which makes role names useless as
-- a way to tell roles apart in any UI or audit log. Unique
-- case-insensitively per guild, same `lower()` functional-index
-- approach `guilds.name`/`guilds.tag` already use (0008_guilds) rather
-- than a `citext` column.
CREATE UNIQUE INDEX guild_roles_name_lower_idx ON guild_roles (guild_id, lower(name));
