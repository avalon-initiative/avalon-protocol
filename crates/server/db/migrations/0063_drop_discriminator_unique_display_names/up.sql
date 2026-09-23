-- Issue #510 (decided): drops the Battle.net/old-Discord-style
-- `display_name#discriminator` handle (#128) in favor of Discord's
-- current scheme — `display_name` itself is the globally-unique,
-- case-insensitive handle. No discriminator, no `#`.
--
-- A deliberate, one-time, pre-public break of the wire/versioning
-- policy's "removing a field bumps version, old versions stay decodable
-- forever" rule (`docs/architecture/protocol-events.md`) — same exception
-- `docs/architecture/nodes.md` already documents for #290: this repo has
-- no real deployed network and zero external integrators yet, so there is
-- no real historical `identity.created`/`profile.updated` data anywhere
-- that this needs to stay compatible with. The dev database is reset as
-- part of this migration landing, not preserved through a collision-
-- resolution pass — see this ticket's own decision record for why.
--
-- `profiles_display_name_discriminator_idx` (0005_friend_handles) is
-- replaced by a uniqueness constraint on `lower(display_name)` alone —
-- same expression-index pattern `guilds_name_lower_idx`/`guilds_tag_lower_idx`
-- (0008_guilds) already establish in this repo. This is also what closes
-- the check-then-insert race #510 flagged: uniqueness is now enforced by
-- this index at the actual INSERT/UPDATE statement, in the same
-- transaction as everything else that write already does — not a
-- separate prior `SELECT` a concurrent writer could race past.
DROP INDEX IF EXISTS profiles_display_name_discriminator_idx;
ALTER TABLE profiles DROP COLUMN discriminator;

CREATE UNIQUE INDEX profiles_display_name_lower_idx
    ON profiles (lower(display_name));
