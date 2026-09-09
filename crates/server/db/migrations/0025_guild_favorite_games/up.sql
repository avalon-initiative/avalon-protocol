-- Guild favorite games: a curated top-5 pin list (issue #207, implementing
-- decision #160), layered on top of #206's real, computed-on-read game
-- affinity breakdown.
--
-- Modeled as a small ordered table, not a capped JSONB/array column like
-- #153's `guilds.links`: unlike a link (label + freeform URL, validated only
-- against static rules), a pin's validity depends on live data in another
-- table (`bindings`, via `guild_members`) — "does this game currently have
-- at least one actively-bound member of this guild" — and each pin also
-- needs its own stable position for reordering. A real table with a
-- `game_id` foreign key lets Postgres enforce "no duplicate pin for the
-- same game" and "the game must actually exist" structurally, and lets
-- `crates/server/src/guilds.rs` reorder with a plain per-row `position`
-- update rather than rewriting/parsing a JSON blob for a two-column shape.
--
-- No CHECK enforcing "at most 5 rows per guild" or "the referenced game
-- currently has an active binding" — both are inherently dynamic
-- (membership/bindings change independently of this table) and are
-- validated in application code at write time
-- (`crates/server/src/guilds.rs::MAX_GUILD_FAVORITE_GAMES`,
-- `set_favorite_games`), same "caps and vocabularies live in code, not the
-- schema" precedent `guild_roles.badge_icon`/`links` already documents.
-- Staleness (a pin whose last bound member has since left) is likewise
-- never stored here — it's computed on every read against the same live
-- `bindings` data #206's breakdown already queries, so it can never drift
-- from reality. Deliberately no durable `guild.favorite_games_*` history
-- beyond the outbox event on each write — this table, like
-- `guild_game_breakdown_public` before it, is current-state-only, not an
-- append-only log.
CREATE TABLE guild_favorite_games (
    guild_id UUID NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    game_id UUID NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    position SMALLINT NOT NULL,
    PRIMARY KEY (guild_id, game_id)
);

-- Enforces "a pin list, not a set" — every pin for a guild has a distinct
-- display position, so ordering is unambiguous without an ORDER BY tiebreak.
CREATE UNIQUE INDEX guild_favorite_games_position_idx
    ON guild_favorite_games (guild_id, position);
