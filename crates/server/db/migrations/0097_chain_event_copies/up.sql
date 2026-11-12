-- Every copy of a chain event a node verified: a signing-key event's hash leaves out its event id, so
-- the same signed event can sit in ledger entries under several ids, and its anchor is the earliest of them.
-- payload_hash is that of the copy's payload with its chain position embedded, as the ledger stores it.
CREATE TABLE identity_chain_event_copies (
    identity_id TEXT NOT NULL,
    event_hash TEXT NOT NULL,
    event_id UUID NOT NULL,
    kind TEXT NOT NULL,
    payload_hash TEXT NOT NULL,
    PRIMARY KEY (identity_id, event_hash, event_id, payload_hash)
);
