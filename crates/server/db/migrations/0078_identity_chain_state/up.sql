-- Per-identity event chain bookkeeping. `identity_chain_events` holds every
-- chained layer-1 event this node knows for an identity (its own and
-- mirrored), and `identity_chain_state` the resolved head of that chain:
-- `seq`/`head_hash` of the last accepted event, and `forked_at_seq` once two
-- chain-critical events conflicted at the same position. Both are derived
-- purely from events, so an index rebuild reproduces them.
CREATE TABLE identity_chain_events (
    identity_id UUID NOT NULL,
    event_id UUID NOT NULL,
    seq BIGINT NOT NULL,
    prev_hash TEXT,
    event_hash TEXT NOT NULL,
    chain_timestamp TIMESTAMPTZ NOT NULL,
    event JSONB NOT NULL,
    PRIMARY KEY (identity_id, event_id)
);

CREATE INDEX identity_chain_events_seq_idx ON identity_chain_events (identity_id, seq);

CREATE TABLE identity_chain_state (
    identity_id UUID PRIMARY KEY,
    seq BIGINT NOT NULL DEFAULT 0,
    head_hash TEXT,
    forked_at_seq BIGINT
);
