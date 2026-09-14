-- Game registration, closing issue #26.
--
-- `games` is a rebuildable projection of `game.registered`
-- (`crates/server/src/games.rs`), same posture as every other projection in
-- this repo: the outbox-written event is canonical, this row is a cache of
-- it. `slug` is enforced unique (409 on collision, matching the
-- `guilds.rs`/migration-0008 unique-index-plus-`is_unique_violation()`
-- pattern) and is never editable once registered — it becomes the `owner`
-- segment of every `GlobalId` the game later mints
-- (`game:<slug>:achievement:<key>`), so renaming it would silently break
-- every historical identifier. Application code enforces the lowercase
-- `[a-z0-9-]` charset; a plain `UNIQUE` constraint is enough here (unlike
-- `guilds.name`/`guilds.tag`, which allow mixed case and so need a
-- `lower()` functional index) since the charset is already fully
-- normalized before it ever reaches this table.
--
-- `status` is a simple string rather than a real state machine — this
-- ticket only ever sets `'active'`. Suspension/revocation/deprecation are
-- a separate, later operator action (see `docs/architecture/issuers.md`).
CREATE TABLE games (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    slug TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    developer TEXT NOT NULL,
    registered_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    status TEXT NOT NULL DEFAULT 'active'
);

-- What a game declared it wants at registration time (`requested_capabilities`
-- on `GameRegistration`, `crates/protocol/src/games.rs`) — a presentation to
-- the player, never a grant. #27 owns the actual grant table; nothing here
-- gives a game access to anything.
CREATE TABLE game_requested_capabilities (
    game_id UUID NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    capability TEXT NOT NULL,
    PRIMARY KEY (game_id, capability)
);

-- The first signing key a game registers with. Shaped so issue #84 (issuer
-- key lifecycle — rotation, multiple keys, revocation, issuer status
-- transitions) can extend rather than replace it: this migration only ever
-- inserts one row per game, at registration time. The node operator never
-- fabricates this key — the registrant supplies it in the registration
-- request; this table only stores and later verifies against it (see
-- `crates/server/src/games.rs::authenticate_game`).
CREATE TABLE issuer_keys (
    key_id UUID PRIMARY KEY,
    game_id UUID NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    algorithm TEXT NOT NULL,
    public_key BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- A milestone-1 stand-in for real issuer-key server-to-server auth pending
-- issue #80's still-open decision on signing scheme/key management
-- generally (see this ticket's own text and `crates/server/src/games.rs`'s
-- module doc comment): `POST /games/{slug}/challenge` inserts a short-lived
-- random nonce here, and any game-authenticated endpoint consumes it via a
-- single-use `DELETE ... RETURNING`, the same ephemeral-ceremony shape
-- `webauthn_ceremonies` (migration 0001) already established for WebAuthn —
-- just without any WebAuthn involved, a game signs the nonce directly with
-- its registered Ed25519 key.
CREATE TABLE game_challenges (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    game_id UUID NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    nonce BYTEA NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL
);
