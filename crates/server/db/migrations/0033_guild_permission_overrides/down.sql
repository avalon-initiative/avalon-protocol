ALTER TABLE guild_channels DROP COLUMN announcement_only;
-- Not reversing the `event_manage` backfill UPDATE above: it's a targeted,
-- idempotent grant (add `event_manage` where `manage_channels` is already
-- present), not new structure, and stripping it back out on a down-migrate
-- would remove authority some role may since have started relying on
-- through this table's own overrides. Same posture other data-carrying
-- migrations in this directory take — only schema is reversed here.
DROP INDEX IF EXISTS guild_permission_overrides_resource_idx;
DROP INDEX IF EXISTS guild_permission_overrides_unique_idx;
DROP TABLE guild_permission_overrides;
