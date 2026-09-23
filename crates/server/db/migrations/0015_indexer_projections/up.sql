-- The indexer's own tables, closing issue #42. See
-- `crates/indexer/src/postgres.rs` and `docs/architecture/query-and-indexing.md`.
--
-- `profiles` is not touched here — it already exists
-- (0001_identity_and_auth, 0005_friend_handles) and becomes a projection in
-- place: `crates/server/src/handlers.rs` stops writing it directly, and
-- `avalon_indexer::projections::profiles` writes it instead. The other
-- three projections below get their own new tables rather than reusing
-- `crates/server`'s existing `friendships`/`guild_members` — those are
-- still written directly by `crates/server/src/friends.rs`/`guilds.rs`,
-- and retargeting that write path is issue #44's job, not this one's.
-- `attestations` has no existing table at all (Epic #30 isn't built yet).

-- Per-event dedup, independent of any projection's own natural-key upsert:
-- a redelivered or replayed (rebuild, issue #43) event is a no-op the
-- moment its id is already here, before any projection table is touched.
CREATE TABLE indexer_applied_events (
    event_id UUID PRIMARY KEY,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Current-state friendship roster built from `friend.accepted` /
-- `friend.removed`. `a < b` mirrors `friendships` (0004_social_graph) so a
-- pair is stored once regardless of who requested whom.
CREATE TABLE indexer_friendships (
    a UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    b UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    since TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (a, b),
    CONSTRAINT indexer_friendships_ordered CHECK (a < b)
);

CREATE INDEX indexer_friendships_b_idx ON indexer_friendships(b);

-- Current-state guild roster built from `guild.member_added` /
-- `guild.member_removed` / `guild.role_changed`. No foreign key into a
-- `guild_roles`-equivalent table on purpose — see this crate's module doc
-- (`crates/indexer/src/projections/guild_rosters.rs`).
CREATE TABLE indexer_guild_members (
    guild_id UUID NOT NULL,
    identity_id UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    role_index INT NOT NULL,
    joined_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (guild_id, identity_id)
);

CREATE INDEX indexer_guild_members_identity_idx ON indexer_guild_members(identity_id);

-- Attestation status cache built from `achievement.issued` /
-- `achievement.revoked` — see
-- `crates/indexer/src/projections/attestations.rs`. `revoked_at IS NULL`
-- means currently valid, same "current status is a cache of the latest
-- event" posture every other current-state projection in this repo uses
-- (e.g. `bindings.ended_at`, 0012_game_bindings).
CREATE TABLE indexer_attestations (
    id UUID PRIMARY KEY,
    issuer TEXT NOT NULL,
    subject UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    achievement TEXT NOT NULL,
    issued_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ
);

CREATE INDEX indexer_attestations_subject_idx ON indexer_attestations(subject);
