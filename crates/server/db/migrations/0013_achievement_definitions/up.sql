-- AchievementDefinition CRUD per game, closing issue #31.
--
-- `achievement_definitions` is a rebuildable projection of
-- `achievement.defined` / `achievement.definition_updated` /
-- `achievement.definition_retired` (`crates/server/src/achievements.rs`),
-- same posture as every other projection in this repo: the outbox-written
-- event is canonical, this row is a cache of it.
--
-- `id` is the definition's namespaced `GlobalId`
-- (`game:<slug>:achievement:<key>`, `crates/protocol/src/ids.rs`) stored as
-- its wire string — immutable and globally unique by construction, so it's
-- the natural primary key. `UNIQUE (game_id, key)` is kept as an explicit,
-- separate constraint anyway (rather than relying on `id`'s own uniqueness)
-- so `POST /games/{slug}/achievements`'s duplicate-key check can use the
-- same `is_unique_violation()` + `constraint()` pattern
-- `crates/server/src/guilds.rs::create_guild` established for
-- name/tag, instead of parsing the id string back apart to explain a
-- conflict.
--
-- `version` starts at 1 and is bumped by every `achievement.definition_updated`
-- — the id never changes, so this is what lets a consumer notice a
-- definition evolved. `retired_at IS NULL` means active, same "current
-- state is a cache of the latest event" posture `bindings.ended_at` and
-- `guild_invites.resolved_at` already use elsewhere in this schema.
-- Retiring never deletes the row or touches any attestation issued against
-- it (issue #31's invariant) — there is no delete path for this table.
CREATE TABLE achievement_definitions (
    id TEXT PRIMARY KEY,
    game_id UUID NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    schema TEXT,
    version INT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    retired_at TIMESTAMPTZ,
    UNIQUE (game_id, key)
);
