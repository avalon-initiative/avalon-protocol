-- Applicant-initiated join requests, symmetric to `guild_invites`
-- (0009_guild_membership) but flowing the other direction: a stranger
-- browsing the Discover board (#154) signals interest in a `recruiting`
-- guild instead of waiting on a manager to invite them. Like
-- `guild_invites`/`friend_requests`, this is a projection, not durable
-- history — no outbox entry for the pending/approved/rejected/withdrawn
-- transition itself, only for the membership-add that approval performs
-- (issue #242).
CREATE TABLE guild_join_requests (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    guild_id UUID NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    applicant UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    message TEXT,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'rejected', 'withdrawn')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    decided_at TIMESTAMPTZ,
    decided_by UUID REFERENCES identities(id) ON DELETE SET NULL
);

-- At most one pending request per (guild, applicant) — re-applying after a
-- rejection or withdrawal is allowed (the old row is no longer pending by
-- then), but applying again while one is already outstanding is a no-op
-- against the existing row rather than a duplicate/error (ticket:
-- "duplicate apply idempotent"), same posture as `guild_invites_pending_idx`.
CREATE UNIQUE INDEX guild_join_requests_pending_idx
    ON guild_join_requests(guild_id, applicant) WHERE status = 'pending';

-- Used by `GET /guilds/{id}/join-requests` to list pending applications for
-- a guild's managers without a full-table scan.
CREATE INDEX guild_join_requests_guild_status_idx
    ON guild_join_requests(guild_id, status);
