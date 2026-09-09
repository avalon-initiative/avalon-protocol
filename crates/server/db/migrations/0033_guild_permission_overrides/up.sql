-- Per-resource guild permission overrides (issue #250, shape decided by
-- #243). Roles keep their existing flat `guild_roles.permissions` base
-- list; this table adds a layer on top — a row granting or denying one
-- `GuildPermission` to one role, scoped to a single channel or event.
--
-- Semantics (enforced in `crates/server/src/guilds.rs::has_resource_permission`
-- / `resolve_resource_permission`, pure-function-tested there): base role
-- permissions are the source of truth when no override row exists for a
-- resource; an explicit deny always wins over a base grant, an explicit
-- grant always wins over a base absence. The guild owner's structural
-- bypass (`guilds.owner`) is unaffected by any override row.
--
-- No FK to `guild_channels`/`guild_events`: `resource_id` is polymorphic
-- (`resource_kind` says which table it names), and an override pointing
-- at a since-deleted resource is meant to be inert rather than requiring
-- cleanup — every endpoint that consults overrides already fetches
-- (and 404s on) the resource first, so an orphaned override is simply
-- never reached. It IS FK'd to `guild_roles (guild_id, name_index)` with
-- `ON DELETE CASCADE`, so deleting a role deletes any overrides written
-- for it — nothing app-level needs to clean those up.
CREATE TABLE guild_permission_overrides (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    guild_id UUID NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    role_index INT NOT NULL,
    resource_kind TEXT NOT NULL CHECK (resource_kind IN ('channel', 'event')),
    resource_id UUID NOT NULL,
    permission TEXT NOT NULL,
    allow BOOLEAN NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (guild_id, role_index) REFERENCES guild_roles (guild_id, name_index) ON DELETE CASCADE
);

-- One override per (role, resource, permission) — setting a new value is
-- an upsert against this key, never an append. Also serves as the lookup
-- index `has_resource_permission`'s exact-match query needs.
CREATE UNIQUE INDEX guild_permission_overrides_unique_idx
    ON guild_permission_overrides (guild_id, role_index, resource_kind, resource_id, permission);

-- "List all overrides for this resource" (Hub role-editing UI, GET
-- /guilds/{id}/permission-overrides) via the same columns' prefix.
CREATE INDEX guild_permission_overrides_resource_idx
    ON guild_permission_overrides (guild_id, resource_kind, resource_id);

-- Preserve today's behavior across the `event_manage` split (issue #250's
-- design section, undoing #169's `manage_channels` workaround): every
-- guild-event endpoint checked `manage_channels` before this migration,
-- so any role that already holds `manage_channels` also gains
-- `event_manage` explicitly here, rather than silently losing
-- event-management authority the moment this migration lands.
UPDATE guild_roles
SET permissions = array_append(permissions, 'event_manage')
WHERE 'manage_channels' = ANY(permissions)
  AND NOT ('event_manage' = ANY(permissions));

-- Issue #250's channel-organization proof point: announcement-only
-- channels. When true, posting requires the `ChannelPost` permission,
-- resolved per-channel through the override layer above rather than a
-- guild-wide grant (see `crates/server/src/guild_messages.rs::send_message`).
-- Defaults to false so every existing channel keeps today's "any member
-- may post" behavior unchanged.
ALTER TABLE guild_channels ADD COLUMN announcement_only BOOLEAN NOT NULL DEFAULT false;
