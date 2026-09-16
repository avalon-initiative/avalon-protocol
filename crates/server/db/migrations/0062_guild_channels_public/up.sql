-- Role-gated view/view_details permissions for guild events and channels
-- (issue #458, implementing #454's decision). Channels have had no
-- non-member visibility concept at all until now — every read required
-- guild membership, unconditionally. This mirrors guild_events.public
-- (#448): defaults to false, so an existing channel's behavior is
-- unchanged (still member-only) until an operator/owner explicitly
-- opts a channel into non-member visibility.
ALTER TABLE guild_channels ADD COLUMN "public" BOOLEAN NOT NULL DEFAULT false;
