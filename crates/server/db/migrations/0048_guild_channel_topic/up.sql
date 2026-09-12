-- Channel topics (issue #276): a short line describing what a channel is
-- for, rendered in the channel header — the Discord/Slack/Zoom-style
-- affordance the guild page's channel view was missing entirely. `NULL`
-- means unset, same "no value" convention `guild_channels.announcement_only`'s
-- sibling fields (`guilds.motd`/`guilds.banner`) already use; an empty
-- string is never stored, it's normalized to `NULL` server-side
-- (`crates/server/src/channels.rs::validate_channel_topic`) on the way in.
ALTER TABLE guild_channels
    ADD COLUMN topic TEXT;
