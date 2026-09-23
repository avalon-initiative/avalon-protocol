-- Friend requests and friendships, closing issue #15.
--
-- A friendship is a durable, network-owned social fact (issue #75's
-- promised-durable reasoning applied to friends, same as guild membership
-- would be) — see docs/projects/backend-server/architecture/social-graph.md. `friend_requests` and
-- `friendships` are both projections, not the record: the canonical history
-- is the `friend.requested` / `friend.accepted` / `friend.removed` events in
-- `protocol_outbox` (crates/server/src/outbox.rs), written in the same
-- transaction as these rows. A declined or withdrawn request is
-- deliberately NOT durable history — it leaves nothing behind worth
-- reconstructing, so the row here is simply marked resolved, not mirrored
-- into an event.

-- `a < b` is enforced so a pair is stored once, regardless of who requested
-- whom, and looked up by either identity without an OR clause.
CREATE TABLE friendships (
    a UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    b UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    since TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (a, b),
    CONSTRAINT friendships_ordered CHECK (a < b)
);

CREATE INDEX friendships_b_idx ON friendships(b);

CREATE TABLE friend_requests (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    "from" UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    "to" UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    requested_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at TIMESTAMPTZ,
    -- NULL while pending; set alongside resolved_at.
    outcome TEXT CHECK (outcome IN ('accepted', 'declined', 'withdrawn'))
);

CREATE INDEX friend_requests_to_idx ON friend_requests("to") WHERE resolved_at IS NULL;
CREATE INDEX friend_requests_from_idx ON friend_requests("from") WHERE resolved_at IS NULL;

-- At most one pending request per direction — re-requesting after a decline
-- is allowed (the old row is resolved by then), but not while one is
-- already outstanding.
CREATE UNIQUE INDEX friend_requests_pending_pair_idx
    ON friend_requests("from", "to") WHERE resolved_at IS NULL;
