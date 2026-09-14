-- A short, player-facing handle for adding friends (issue #128) — a
-- Battle.net/Discord-style `display_name#1234` pair, resolved by *exact*
-- match only, never partial/fuzzy (that's the separate player-discovery
-- question, issue #129).
--
-- The discriminator is a 4-digit string generated once, server-side, when a
-- profile is created (or when a display-name change collides with an
-- existing handle — see `crates/server/src/handlers.rs`); it is not
-- player-chosen and does not change on its own. `display_name` can still
-- change (`PATCH /me`), so the uniqueness constraint covers the
-- (display_name, discriminator) pair, not either column alone.
-- The DEFAULT exists as a safety net for any insert path that doesn't set
-- discriminator explicitly (e.g. test fixtures that seed a profile directly
-- via SQL) — the real registration path
-- (`handlers::generate_unique_discriminator`) always generates and binds
-- one explicitly rather than relying on this, since it needs to retry on a
-- collision the DEFAULT can't detect.
ALTER TABLE profiles ADD COLUMN discriminator TEXT
    DEFAULT lpad((floor(random() * 10000))::int::text, 4, '0');

-- Backfill existing rows. Collisions are possible in principle but
-- vanishingly unlikely against this dev dataset's size (~10000 values per
-- distinct display_name); this is dev-only backfill, not a production
-- migration path.
UPDATE profiles SET discriminator = lpad((floor(random() * 10000))::int::text, 4, '0')
    WHERE discriminator IS NULL;

ALTER TABLE profiles ALTER COLUMN discriminator SET NOT NULL;

CREATE UNIQUE INDEX profiles_display_name_discriminator_idx
    ON profiles (display_name, discriminator);
