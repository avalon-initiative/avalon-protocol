-- Issue #448: a per-event visibility flag so a guild can show some events
-- to prospective members (a public "come play with us" event) while
-- keeping others member-only (officer planning, loot council) even while
-- the guild itself is `public` (#449/#455). Defaults to `false` — every
-- existing event stays exactly as visible as it is today (member-only);
-- nothing becomes newly exposed just by this migration landing.
ALTER TABLE guild_events
    ADD COLUMN "public" BOOLEAN NOT NULL DEFAULT false;
