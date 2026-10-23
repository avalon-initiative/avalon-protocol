-- Keys and passkeys a recovery revoked remember which recovery event did it, so displacing that
-- recovery restores exactly those rows and never one an unrelated event revoked at the same time.
ALTER TABLE indexer_identity_signing_keys ADD COLUMN revoked_by_event_id UUID;
ALTER TABLE indexer_identity_passkeys ADD COLUMN revoked_by_event_id UUID;
