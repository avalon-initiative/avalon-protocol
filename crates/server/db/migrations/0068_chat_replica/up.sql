-- Issue #540: async at-rest replication of guild chat/conversation
-- history to at least one additional node (#535's decision), so a node's
-- loss doesn't take its chat history with it.
--
-- Deliberately a separate, denormalized, foreign-key-free table, not an
-- insert into `guild_messages`/`conversation_messages` themselves:
-- `guild_messages.channel_id`/`.author` reference `guild_channels(id)`/
-- `identities(id)`, which a node receiving a replicated message may not
-- itself hold a copy of (today's architecture replicates core
-- identity/guild data only to a node that's also authoring it directly —
-- see docs/architecture/nodes.md; the indexer-projection replication
-- #506 built covers membership rosters, not the core
-- `guild_channels`/`identities` tables these foreign keys point at). A
-- disaster-recovery copy that can fail to insert because of an unrelated
-- referential-integrity gap on the replica is not a copy an operator can
-- rely on, so this table intentionally carries no foreign keys at all —
-- it exists purely to be read back after the originating node is lost,
-- never to be joined against or served through the normal live-read path.
CREATE TABLE guild_messages_replica (
    id UUID PRIMARY KEY,
    channel_id UUID NOT NULL,
    author UUID NOT NULL,
    body TEXT NOT NULL,
    sent_at TIMESTAMPTZ NOT NULL,
    deleted_at TIMESTAMPTZ,
    replicated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX guild_messages_replica_channel_idx
    ON guild_messages_replica (channel_id, sent_at DESC);

CREATE TABLE conversation_messages_replica (
    id UUID PRIMARY KEY,
    conversation_id UUID NOT NULL,
    author UUID NOT NULL,
    body TEXT NOT NULL,
    sent_at TIMESTAMPTZ NOT NULL,
    replicated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX conversation_messages_replica_conversation_idx
    ON conversation_messages_replica (conversation_id, sent_at DESC);
