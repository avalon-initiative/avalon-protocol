DELETE FROM identity_chain_events a USING identity_chain_events b
WHERE a.identity_id = b.identity_id AND a.event_id = b.event_id AND a.ctid > b.ctid;
ALTER TABLE identity_chain_events DROP CONSTRAINT identity_chain_events_pkey;
ALTER TABLE identity_chain_events ADD PRIMARY KEY (identity_id, event_id);
