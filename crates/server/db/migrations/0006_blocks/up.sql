-- Blocking, closing issue #97.
--
-- Unlike friendship (0004_social_graph), a block is never durable protocol
-- history and never a projection of an outbox event: it's unilateral (no
-- consent from the blocked party), and it must stay private to the blocker
-- alone. Avalon's settlement layer is a public transparency log anyone can
-- mirror (ADR #70/#93) — putting "identity A blocked identity B" there,
-- even hashed, would let every mirror operator (and eventually the blocked
-- party via log analysis) learn who blocked whom. This table is
-- avalon-server's own application state, the same relationship presence has
-- to durable history (ADR #78) — losing it means users re-block people,
-- an acceptable failure mode, not a fact anyone needs to prove later.
--
-- Deliberately not symmetric like `friendships` (`a < b`): blocking has a
-- direction (blocker vs. blocked), so both columns are named for their
-- role rather than canonicalized into an ordered pair.
CREATE TABLE blocks (
    blocker UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    blocked UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (blocker, blocked),
    CONSTRAINT blocks_no_self_block CHECK (blocker <> blocked)
);

-- Looked up from the "is there a block between these two identities, in
-- either direction" check that presence/friend-request enforcement runs —
-- the primary key already covers `blocker`-first lookups, this covers the
-- reverse direction.
CREATE INDEX blocks_blocked_idx ON blocks(blocked);
