-- Guild icon (issue #246): a small badge image, independent of `banner`
-- (issue #153, #20 migration). Same shape and posture as `banner` — plain
-- nullable TEXT, `NULL` meaning "not set", validated and length-capped in
-- application code (`crates/server/src/guilds.rs::validate_guild_icon`),
-- not a database CHECK, same reasoning `banner`/`motd` already documented
-- in the #20 migration. A banner is a wide cover image; an icon is a
-- compact identity mark (recruitment card, member-list-style avatar,
-- external integrator badge) — different visual roles, so a second field
-- rather than deriving one from the other.
ALTER TABLE guilds
    ADD COLUMN icon TEXT;
