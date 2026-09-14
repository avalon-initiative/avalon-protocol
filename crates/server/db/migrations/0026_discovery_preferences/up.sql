-- The opt-in half of #129's two-tier user discovery decision (issue
-- #205). #204's always-on scoped surfacing (friends-of-friends, mutual
-- guild) needs no preference at all; this is the other tier — a user
-- explicitly opting into open name/handle search via `GET /identities/search`.
--
-- Same shape as `presence_preferences.hide_playing`
-- (`crates/server/db/migrations/0017_presence_preferences`): one row per
-- identity, created lazily on first toggle
-- (`crates/server/src/discovery.rs::set_discoverable`'s upsert) rather than
-- eagerly for every identity at registration — absence means "not
-- discoverable", the default, so every identity is off by default with no
-- backfill required. Deliberately not a `ProtocolEvent`/durable history —
-- same reasoning `docs/architecture/privacy.md` already gives for
-- `hide_playing`: a user preference that removes something from view
-- entirely, not a fact about the network worth a durable record.
CREATE TABLE discovery_preferences (
    identity_id UUID PRIMARY KEY REFERENCES identities(id) ON DELETE CASCADE,
    discoverable BOOLEAN NOT NULL DEFAULT false
);
