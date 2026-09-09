-- Adds a description and a small fixed-vocabulary visual badge (icon +
-- color) to guild roles, closing issue #152. Additive/backward-compatible
-- with #20's existing `guild_roles` rows: every new column is NOT NULL
-- with a DEFAULT, so existing rows get a real (not null) description/badge
-- for free, with no backfill step and no null-handling branch needed on
-- the read path.
--
-- `badge_icon`/`badge_color` are stored as two TEXT columns rather than a
-- single JSON/composite column or a database enum, same reasoning
-- `guild_roles.permissions` already documents: the fixed milestone-1
-- vocabulary (`crates/protocol/src/guilds.rs`'s `RoleBadgeIcon`/
-- `RoleBadgeColor`) is enforced in application code, not a database CHECK,
-- so growing the vocabulary later doesn't need a migration to loosen a
-- constraint. Two plain columns (rather than one) also leave room for a
-- future richer badge system to add columns beside these without
-- reshaping this pair.
ALTER TABLE guild_roles
    ADD COLUMN description TEXT NOT NULL DEFAULT '',
    ADD COLUMN badge_icon TEXT NOT NULL DEFAULT 'star',
    ADD COLUMN badge_color TEXT NOT NULL DEFAULT 'gray';
