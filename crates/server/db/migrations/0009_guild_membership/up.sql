-- Membership lifecycle backing `GuildMember`, closing issue #21. See
-- `docs/projects/backend-server/architecture/guilds.md` and issues #74/#75 for why membership is a
-- durable, network-owned fact and this table (like `guild_invites` below)
-- is a rebuildable projection, not the record — the canonical history is
-- `guild.member_added` / `guild.member_removed` / `guild.role_changed` in
-- `protocol_outbox` (crates/server/src/outbox.rs), written in the same
-- transaction as the row change here.

-- Whether a guild accepts open joins or requires an invite. Default
-- `invite_only` matches the ticket; `open` is the only other value a guild
-- owner/manager can opt into via `PATCH /guilds/{id}` in a later ticket —
-- this migration only adds the column and its constraint, #21's own
-- endpoints don't expose changing it.
ALTER TABLE guilds ADD COLUMN join_policy TEXT NOT NULL DEFAULT 'invite_only'
    CHECK (join_policy IN ('invite_only', 'open'));

-- One row per (guild, identity); a member has exactly one role, enforced by
-- `role_index` being a plain column rather than a join table. The
-- guild_id/role_index pair must reference a real row in `guild_roles`, so a
-- member's role can never point at a role that doesn't exist for this
-- guild. `create_guild` (issue #20's flow, extended here) inserts the
-- owner's own row at `role_index = 0` in the same transaction as the guild
-- and its starter roles, so a guild is never observably ownerless of a
-- membership row.
CREATE TABLE guild_members (
    guild_id UUID NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    identity_id UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    role_index INT NOT NULL,
    joined_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (guild_id, identity_id),
    FOREIGN KEY (guild_id, role_index) REFERENCES guild_roles(guild_id, name_index)
);

-- Used by `GET /me/guilds` to look up a caller's memberships without an
-- OR-scan of every guild's roster.
CREATE INDEX guild_members_identity_idx ON guild_members(identity_id);

-- Guild invites, mirroring `friend_requests` (0004_social_graph) — a
-- pending invite is a projection, resolved (accepted/declined) with no
-- durable event of its own, same reasoning as a declined/withdrawn friend
-- request. `outcome` is NULL while pending, set alongside `resolved_at`.
CREATE TABLE guild_invites (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    guild_id UUID NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    "to" UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    "from" UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at TIMESTAMPTZ,
    outcome TEXT CHECK (outcome IN ('accepted', 'declined'))
);

-- At most one pending invite per (guild, to) — re-inviting after a decline
-- is allowed (the old row is resolved by then), but issuing a second
-- invite while one is already outstanding is a no-op against the existing
-- row rather than a duplicate/error (ticket: "duplicate invite idempotent").
CREATE UNIQUE INDEX guild_invites_pending_idx
    ON guild_invites(guild_id, "to") WHERE resolved_at IS NULL;
