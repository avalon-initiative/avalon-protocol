-- Guild MOTD and small metadata fields (issue #153): a message-of-the-day,
-- a banner URL, a capped list of external links, and a recruiting flag.
-- Additive/backward-compatible with #20's existing `guilds` rows, same
-- posture as #152's `guild_roles` migration: every new column is either
-- nullable (no backfill meaning) or NOT NULL with a DEFAULT, so existing
-- rows read back with sensible values and no null-handling branch is
-- needed beyond the ones the read path already has for `description`.
--
-- `motd`/`banner` are plain nullable TEXT — `NULL` means "not set" (same
-- "empty string clears it" convention `crates/server/src/handlers.rs`
-- already uses for `profiles.bio`/`profiles.pronouns`), validated and
-- length-capped in application code
-- (`crates/server/src/guilds.rs::validate_guild_motd`/`validate_guild_banner`),
-- not by a database CHECK — same reasoning `guild_roles.badge_icon`/
-- `badge_color` already documents for why these vocabularies/caps live in
-- code, not the schema.
--
-- `links` is `JSONB`, not a separate join table: it's a small, ordered,
-- guild-owned list of `{ label, url }` pairs (capped at 5 entries by
-- `crates/server/src/guilds.rs::MAX_GUILD_LINKS`), read and written as one
-- unit alongside the rest of a guild update — same "one JSONB column,
-- application code is the source of truth for shape and caps" precedent
-- `outbox.event`/`ledger.payload` already set, rather than introducing a
-- `guild_links` table for what's never queried independently of its guild.
--
-- `recruiting` is a plain boolean, defaulting to `false` — issue #154's
-- discovery board reads this column directly (`WHERE recruiting`), so it
-- needs to be a real, indexable column rather than folded into `links` or
-- some other blob.
ALTER TABLE guilds
    ADD COLUMN motd TEXT,
    ADD COLUMN banner TEXT,
    ADD COLUMN links JSONB NOT NULL DEFAULT '[]',
    ADD COLUMN recruiting BOOLEAN NOT NULL DEFAULT false;

-- Issue #154 discovers guilds by `recruiting = true`; a partial index keeps
-- that scan cheap without indexing the (usually false) common case.
CREATE INDEX guilds_recruiting_idx ON guilds (recruiting) WHERE recruiting;
