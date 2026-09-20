-- Epic #623, issue #634: cross-node login pending-request lifecycle,
-- structurally close to 0042_device_pairings but explicitly cross-node —
-- approval never requires a live session on the requesting node itself
-- (unlike #307's same-node pairing). `identity_id`/`session_token` stay
-- nullable until a verified `avalon_protocol::cross_node_login::CrossNodeLoginGrant`
-- is submitted against a pending row.
CREATE TABLE cross_node_login_requests (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    request_code TEXT UNIQUE NOT NULL,
    user_code TEXT UNIQUE NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'denied', 'expired')),
    -- This node's own base_url at request-creation time, shown to the
    -- approver as part of requesting_context and re-checked against the
    -- submitted grant's own destination_base_url at verification time
    -- (issue #610's destination-binding lesson, applied here).
    requesting_base_url TEXT NOT NULL,
    identity_id UUID REFERENCES identities(id) ON DELETE CASCADE,
    session_token TEXT REFERENCES sessions(token) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    last_polled_at TIMESTAMPTZ
);

CREATE INDEX cross_node_login_requests_user_code_idx ON cross_node_login_requests(user_code);

-- Anti-replay gate for CrossNodeLoginGrant, same shape 0066's
-- consumed_continuation_nonces already established: a grant's nonce may
-- only ever be accepted once, and expires_at mirrors the grant's own
-- claimed expiry so an opportunistic sweep keeps this bounded without a
-- separate scheduled job.
CREATE TABLE consumed_cross_node_login_nonces (
    nonce UUID PRIMARY KEY,
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX consumed_cross_node_login_nonces_expires_at_idx
    ON consumed_cross_node_login_nonces (expires_at);
