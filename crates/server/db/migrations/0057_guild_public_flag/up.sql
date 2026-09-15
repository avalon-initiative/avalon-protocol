-- Issue #449 (decided) / #455 (implementation): a guild's "recruiting"
-- (discovery board + join-request eligibility) and "publicly viewable"
-- (roster/event visibility to non-members) are independent settings, not
-- one boolean doing both jobs. `recruiting` (added by #153, migration
-- 0020) keeps its existing meaning; this adds a second, independent
-- `public` column that `list_members`'s roster-visibility override now
-- keys off instead.
--
-- `public` is a reserved-ish identifier in Postgres (the default schema
-- name) but not a reserved keyword, so it works unquoted in DDL — quoted
-- here and in queries anyway for the same "avoid any ambiguity" reasoning
-- `guild_invites."to"` already established for a case that does need it.
--
-- No backfill/migration-safety concerns: this database is dev-only right
-- now with nothing durable to preserve (per #449's decision comment), so
-- a plain `DEFAULT false` is fine — no existing guild silently becomes
-- public just because it was already recruiting.
ALTER TABLE guilds
    ADD COLUMN "public" BOOLEAN NOT NULL DEFAULT false;
