-- A player's own opt-out of `playing` being shown in their presence,
-- closing part of issue #16 ("a player can independently opt out of
-- `playing` being shown at all, separate from any game's capability
-- grant").
--
-- Deliberately not part of the ephemeral `PresenceStore`
-- (`crates/server/src/presence.rs`) even though it gates a presence field:
-- this is a durable player *preference* ("never show what I'm playing"),
-- not a realtime fact like "playing right now" — it must survive a server
-- restart the same way any other player setting would, unlike the
-- ephemeral store itself (ADR #78). It is also not a `ProtocolEvent`: same
-- reasoning `docs/architecture/privacy.md` already gives for visibility
-- settings generally ("player state, not durable protocol history, unless
-- a later decision promotes them").
--
-- One row per identity, created lazily on first opt-out (see
-- `crates/server/src/presence.rs::set_hide_playing`'s upsert) rather than
-- eagerly for every identity at registration — absence means "not hidden",
-- the default, same "missing means the default, never invented" posture
-- `PresenceStore::get` already uses for a missing presence entry.
CREATE TABLE presence_preferences (
    identity_id UUID PRIMARY KEY REFERENCES identities(id) ON DELETE CASCADE,
    hide_playing BOOLEAN NOT NULL DEFAULT false
);
