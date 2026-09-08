-- Guild chat channels and messages, closing issue #22.
--
-- `guild_channels` is a rebuildable projection of durable guild-structure
-- history (`guild.channel_created`, `guild.channel_renamed`,
-- `guild.channel_archived` — see `crates/server/src/channels.rs`), same
-- pattern as `guilds`/`guild_roles` in migration 0008: nothing here is
-- treated as canonical, the outbox-written events are.
--
-- `guild_messages` is deliberately NOT a projection of anything durable.
-- Ordinary chat is high-volume, non-interoperable state that never goes
-- through the outbox or `SettlementProvider::commit` — same reasoning
-- already applied to presence (`crates/server/src/presence.rs`, which also
-- never touches those). Rows here are retained up to a configurable
-- per-channel cap (`GUILD_CHANNEL_MESSAGE_CAP` env var, default 10,000)
-- with the oldest pruned once the cap is exceeded — see
-- `crates/server/src/guild_messages.rs`'s pruning query. Nothing about this
-- table is promised-durable or reconstructable from protocol events; no
-- client should assume chat history is permanent.
--
-- Integration note (issues #21 and #22 landing together): the membership
-- checks in `crates/server/src/channels.rs` / `guild_messages.rs` query a
-- `guild_members` table that this migration deliberately does NOT create —
-- that table belongs to issue #21's own migration. This migration only
-- owns `guild_channels` and `guild_messages`.
CREATE TABLE guild_channels (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    guild_id UUID NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    -- NULL = active. Archival is a soft flag, not a delete: an archived
    -- channel's message history stays queryable, it just stops accepting
    -- new posts (`crates/server/src/guild_messages.rs::send_message`).
    archived_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX guild_channels_guild_id_idx ON guild_channels (guild_id);

CREATE TABLE guild_messages (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    channel_id UUID NOT NULL REFERENCES guild_channels(id) ON DELETE CASCADE,
    author UUID NOT NULL REFERENCES identities(id),
    body TEXT NOT NULL CHECK (char_length(body) <= 4000),
    sent_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Newest-first pagination (`GET .../messages?before=&limit=`) and
-- cap-based pruning both order by (channel_id, sent_at DESC, id DESC) —
-- this index serves both.
CREATE INDEX guild_messages_channel_sent_at_idx
    ON guild_messages (channel_id, sent_at DESC, id DESC);
