-- The Game Registry's binding-status cache (issue #261, first slice of the
-- epic-sized #89), built from `game.binding_established` /
-- `game.binding_ended` — see
-- `crates/indexer/src/projections/game_bindings.rs` and
-- `docs/architecture/game-registry.md`.
--
-- Kept as its own table rather than reusing `bindings` (0012_game_bindings)
-- for the same reason `indexer_friendships`/`indexer_guild_members`/
-- `indexer_attestations` (0015_indexer_projections) already get their own
-- tables instead of the existing `friendships`/`guild_members`: `bindings`
-- is still written directly by `crates/server/src/connections.rs` at
-- request time, and retargeting that write path onto the indexer is #44's
-- job, not this one's.
--
-- No foreign keys into `identities`/`games` on purpose, same posture
-- `indexer_guild_members` already takes for `guild_id` — this table only
-- needs to survive replay/rebuild from the event log, never to enforce
-- referential integrity a settlement-layer projection doesn't own.
CREATE TABLE indexer_game_bindings (
    id UUID PRIMARY KEY,
    identity_id UUID NOT NULL,
    game_id UUID NOT NULL,
    established_at TIMESTAMPTZ NOT NULL,
    ended_at TIMESTAMPTZ
);

-- "players" (active) and "total players ever" (all) both filter by
-- `game_id` and count distinct `identity_id` — see
-- `crates/indexer/src/registry.rs::compute_for_game`.
CREATE INDEX indexer_game_bindings_game_id_idx ON indexer_game_bindings (game_id);
