-- Guild CRUD and roles, closing issue #20 (see issue #74/#75 for why guilds
-- are network-level primitives, not game-owned, and why their history is
-- canonical, the `guilds`/`guild_roles` rows a rebuildable projection).
--
-- `name` and `tag` are unique network-wide, case-insensitively — enforced
-- with `lower()` functional unique indexes rather than `citext` since no
-- other table in this repo uses that extension yet. `tag` is additionally
-- length-bounded (2-5 chars) at the database level, matching the ticket.
--
-- A guild always has exactly one owner. That's enforced here by `owner`
-- being a plain NOT NULL column (never nullable, never multi-valued) rather
-- than a separate "owners" join table — ownership transfer
-- (`guild.owner_transferred`) is a single UPDATE of this column inside a
-- transaction that also enqueues the event, so the invariant can never be
-- observed broken (zero or two owners) between statements.
CREATE TABLE guilds (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    tag TEXT NOT NULL CHECK (char_length(tag) BETWEEN 2 AND 5),
    description TEXT NOT NULL DEFAULT '',
    owner UUID NOT NULL REFERENCES identities(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX guilds_name_lower_idx ON guilds (lower(name));
CREATE UNIQUE INDEX guilds_tag_lower_idx ON guilds (lower(tag));

-- Guild-owned roles, not game-owned (per the ticket). `name_index` is a
-- per-guild sequence starting at 0, matching `GuildRole.name_index` in
-- `crates/protocol/src/guilds.rs`. `permissions` is a fixed, non-extensible
-- set in milestone 1 (`manage_guild`, `manage_roles`, `manage_members`,
-- `manage_channels`) — stored as a text array rather than a bitset column
-- since Postgres has no native bitset type and this keeps the row
-- human-readable; the fixed vocabulary is enforced in application code
-- (`crates/server/src/guilds.rs`), not by a database CHECK, so adding a
-- permission later doesn't require a migration to loosen a constraint.
CREATE TABLE guild_roles (
    guild_id UUID NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    name_index INT NOT NULL,
    name TEXT NOT NULL,
    permissions TEXT[] NOT NULL DEFAULT '{}',
    PRIMARY KEY (guild_id, name_index)
);

-- Opt-in, non-owning association between a guild and a game (see
-- `docs/architecture/guilds.md` "Game as client"). A game never rows here
-- itself — only a guild manager, via `POST /guilds/{id}/games/{game_id}`.
CREATE TABLE guild_game_associations (
    guild_id UUID NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    game_id UUID NOT NULL,
    associated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (guild_id, game_id)
);
