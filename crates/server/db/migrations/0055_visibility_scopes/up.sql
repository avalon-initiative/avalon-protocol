-- Issue #87: player/guild-controlled visibility settings for the two read
-- paths that had none before this — presence (previously a hardcoded
-- friends-only rule, now that same default but player-configurable) and
-- guild rosters (previously unscoped: any authenticated session could read
-- any guild's full roster). Values are `avalon_protocol::permissions::Visibility`'s
-- own wire strings, validated in application code
-- (`crate::visibility::parse_visibility`), not a database CHECK constraint
-- — same posture `guilds.join_policy` already takes on this exact
-- question, so a new `Visibility` variant never needs a migration to
-- loosen a constraint.
ALTER TABLE profiles
    ADD COLUMN presence_visibility TEXT NOT NULL DEFAULT 'friends';

ALTER TABLE guilds
    ADD COLUMN roster_visibility TEXT NOT NULL DEFAULT 'guild_members';
