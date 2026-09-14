-- Game bindings and capability grants, closing issues #83 and #27.
--
-- `bindings` is a rebuildable projection of `game.binding_established` /
-- `game.binding_ended` (`crates/server/src/connections.rs`), same posture as
-- every other projection in this repo: the outbox-written event is
-- canonical, this row is a cache of it. See
-- `docs/architecture/bindings.md`.
--
-- A binding never has game-specific fields (no character, race, class,
-- level, appearance, progression) — that data stays in the game's own
-- database, by design. `ended_at IS NULL` means active. The partial unique
-- index (rather than a plain `UNIQUE (identity_id, game_id)`) allows a
-- player to re-bind to the same game after ending a previous binding —
-- `POST /games/{slug}/connect` is idempotent against an *active* binding
-- only, not against binding history, and ending a binding must not block
-- ever reconnecting.
CREATE TABLE bindings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    identity_id UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    game_id UUID NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    established_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at TIMESTAMPTZ
);

CREATE UNIQUE INDEX bindings_one_active_per_identity_game
    ON bindings (identity_id, game_id)
    WHERE ended_at IS NULL;

-- One row per capability a player has actually approved for a game, scoped
-- to the binding it hangs off — no active binding, no grants (enforced at
-- the application layer: `binding_id` always references a still-active
-- binding at insert time, and `DELETE /games/{slug}/connect` revokes every
-- active grant under the binding it ends, in the same transaction).
-- `revoked_at IS NULL` means active, same "current state is a cache of the
-- latest event" posture as `bindings.ended_at`.
CREATE TABLE permission_grants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    binding_id UUID NOT NULL REFERENCES bindings(id) ON DELETE CASCADE,
    capability TEXT NOT NULL,
    granted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ
);

CREATE UNIQUE INDEX permission_grants_one_active_per_binding_capability
    ON permission_grants (binding_id, capability)
    WHERE revoked_at IS NULL;
