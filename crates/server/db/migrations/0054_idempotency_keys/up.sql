-- Issue #47: a generic idempotency cache for mutations that need "retried
-- safely" semantics beyond a request-level retry — keyed by the calling
-- integrator plus a client-supplied Idempotency-Key header plus the
-- endpoint it was sent to, so the same key can't collide across two
-- different mutations. Stores the exact successful JSON response the first
-- attempt produced, so a retried request replays that response instead of
-- re-executing the mutation. Rows are never updated, only inserted once
-- (ON CONFLICT DO NOTHING at the call site) and never pruned automatically
-- yet — bounded growth is a follow-up, not this ticket's scope.
CREATE TABLE idempotency_keys (
    integrator_id UUID NOT NULL,
    idempotency_key TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    response_body JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (integrator_id, idempotency_key, endpoint)
);
