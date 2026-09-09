-- Guild events calendar + RSVP, closing issue #169.
--
-- Durability call (made explicitly, per the ticket, rather than assumed):
-- `guild_events` and `guild_event_rsvps` are BOTH projection-only tables.
-- There is no `guild.event_*` protocol event kind for creating,
-- rescheduling, or cancelling an event, and no per-RSVP event either —
-- neither table's writes go through the outbox or `SettlementProvider`.
--
-- This is a deliberate departure from `guild_channels` (migration 0010),
-- which treats channel structure as durable history behind
-- `guild.channel_created`/`renamed`/`archived`. A scheduled event doesn't
-- have the same "durable structure" character a channel does: a raid
-- night gets rescheduled or cancelled repeatedly, and that churn isn't
-- history worth preserving forever any more than the chat that happens in
-- a channel is (`guild_messages`, same migration 0010). So the whole
-- calendar feature — event rows AND RSVP rows — gets the "hot state, not
-- history" treatment `guild_messages` and presence already established,
-- not the "structure is durable, content isn't" split channels/messages
-- use. See `crates/server/src/guild_events.rs`'s module doc comment and
-- `docs/architecture/guilds.md` for the same reasoning stated for readers
-- of the architecture doc.
--
-- Nothing here is promised-durable or reconstructable from protocol
-- events; no client should assume the guild calendar is permanent or
-- that a deleted event's RSVPs are recoverable.
CREATE TABLE guild_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    guild_id UUID NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    -- Optional discussion channel. ON DELETE SET NULL: deleting the
    -- channel shouldn't delete the event, it just stops being tied to a
    -- channel for discussion.
    channel_id UUID REFERENCES guild_channels(id) ON DELETE SET NULL,
    title TEXT NOT NULL,
    description TEXT,
    starts_at TIMESTAMPTZ NOT NULL,
    ends_at TIMESTAMPTZ,
    created_by UUID NOT NULL REFERENCES identities(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Calendar listing is always "events for this guild in some date range,
-- ordered by start time" — this index serves both.
CREATE INDEX guild_events_guild_starts_at_idx ON guild_events (guild_id, starts_at);

CREATE TABLE guild_event_rsvps (
    event_id UUID NOT NULL REFERENCES guild_events(id) ON DELETE CASCADE,
    identity_id UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    -- 'going' | 'maybe' | 'not_going' — see
    -- avalon_protocol::guilds::RsvpStatus. One row per (event, identity);
    -- a member updates their own row in place (upsert), never inserts a
    -- second row for the same event.
    status TEXT NOT NULL,
    responded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (event_id, identity_id)
);
