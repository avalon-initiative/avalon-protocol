-- Direct/small-group conversations, closing issue #102.
--
-- `Conversation` (`crates/protocol/src/social.rs`) is the identity-to-identity
-- sibling of `GuildChannel` (migration 0010): pure structure, no game
-- reference. `conversations` holds one row per distinct participant set;
-- `conversation_participants` is the join table. `conversation_messages`
-- mirrors `guild_messages`'s "deliberately NOT a rebuildable projection"
-- shape exactly — high-volume, non-interoperable chat content that never
-- goes through the outbox or `SettlementProvider::commit`. See
-- `crates/server/src/conversations.rs`.
--
-- Idempotent creation for a given participant set (`POST /conversations`
-- must return the existing conversation rather than creating a duplicate)
-- is enforced here, not just in application code: `participants_key` is a
-- deterministic, sorted, comma-joined rendering of the participant id set,
-- computed in Rust (`crate::conversations::participants_key`) and given a
-- UNIQUE constraint. Two requests for the same set of identities — in any
-- order — collide on this constraint; the handler catches that the same
-- way `blocks::create_block` catches a duplicate-block unique violation.
CREATE TABLE conversations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    participants_key TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE conversation_participants (
    conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    identity_id UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    PRIMARY KEY (conversation_id, identity_id)
);

-- Serves "the caller's own conversation list" (`GET /conversations`) and
-- the participant-membership check every conversation endpoint runs.
CREATE INDEX conversation_participants_identity_id_idx
    ON conversation_participants (identity_id);

-- Not protocol history — see module comment above and
-- `crates/server/src/conversations.rs`. Retained up to a configurable
-- per-conversation cap (`CONVERSATION_MESSAGE_CAP` env var, default 10,000,
-- same default `guild_messages` uses), oldest pruned (hard-deleted, no
-- archive tier — #253's archive work was a guild-chat-specific follow-up
-- decision, not part of this ticket) once the cap is exceeded.
CREATE TABLE conversation_messages (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    author UUID NOT NULL REFERENCES identities(id),
    body TEXT NOT NULL CHECK (char_length(body) <= 4000),
    sent_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Newest-first pagination (`GET .../messages?before=&limit=`) and
-- cap-based pruning both order by (conversation_id, sent_at DESC, id DESC) —
-- same index shape as `guild_messages_channel_sent_at_idx`.
CREATE INDEX conversation_messages_conversation_sent_at_idx
    ON conversation_messages (conversation_id, sent_at DESC, id DESC);
