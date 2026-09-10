-- Archive tier for cap-pruned guild chat messages (issue #253,
-- implementing #193's "count cap + archive + long-window expiry" decision).
--
-- `guild_messages` stays a small hot table bounded by
-- `GUILD_CHANNEL_MESSAGE_CAP` (migration 0010) for read performance.
-- `guild_messages_archive` is where `prune_channel`
-- (`crates/server/src/guild_messages.rs`) now moves rows that fall out of
-- that cap, instead of hard-deleting them outright. Same row shape as
-- `guild_messages` plus `archived_at` (when the row was moved here).
--
-- This table is still NOT durable protocol history — same "ephemeral,
-- non-interoperable state" classification #74/#75 already give
-- `guild_messages` itself (see that migration's own comment, and
-- `crates/server/src/guild_messages.rs`'s module doc comment). It never
-- goes through the outbox or `SettlementProvider::commit`. It exists
-- purely to give an operational, longer-than-the-hot-cap memory to guild
-- chat, not to promote chat into something a game or another identity
-- could ever need to verify.
--
-- Rows here are hard-deleted for good once `archived_at` is older than the
-- configurable long retention window (`GUILD_MESSAGE_ARCHIVE_RETENTION_DAYS`
-- env var, default 730 days / ~2 years — see `expire_archive` in
-- `crates/server/src/guild_messages.rs`). That expiry is a real, final
-- hard delete: the archive's retention window is a promise about *when*
-- deletion happens, not a claim of permanence beyond it.
CREATE TABLE guild_messages_archive (
    id UUID PRIMARY KEY,
    channel_id UUID NOT NULL REFERENCES guild_channels(id) ON DELETE CASCADE,
    author UUID NOT NULL REFERENCES identities(id),
    body TEXT NOT NULL CHECK (char_length(body) <= 4000),
    sent_at TIMESTAMPTZ NOT NULL,
    archived_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Serves the archive's own newest-first, cursor-paginated read endpoint
-- (`GET .../channels/{cid}/archive?before=&limit=`), same ordering
-- `guild_messages_channel_sent_at_idx` already serves for the live table.
CREATE INDEX guild_messages_archive_channel_sent_at_idx
    ON guild_messages_archive (channel_id, sent_at DESC, id DESC);

-- Serves the expiry query's `WHERE archived_at < cutoff` scan.
CREATE INDEX guild_messages_archive_archived_at_idx
    ON guild_messages_archive (archived_at);
