ALTER TABLE recovery_approvals DROP COLUMN signature;
ALTER TABLE recovery_approvals DROP COLUMN signing_key_id;
ALTER TABLE recovery_requests DROP COLUMN new_signing_public_key;
ALTER TABLE identity_keys DROP COLUMN announced_at;
