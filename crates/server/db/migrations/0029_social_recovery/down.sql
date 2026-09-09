ALTER TABLE webauthn_ceremonies DROP CONSTRAINT webauthn_ceremonies_kind_check;
ALTER TABLE webauthn_ceremonies
    ADD CONSTRAINT webauthn_ceremonies_kind_check
    CHECK (kind IN ('registration', 'authentication', 'add_passkey'));

DROP TABLE recovery_approvals;
DROP TABLE recovery_requests;
DROP TABLE recovery_guardians;
DROP TABLE recovery_guardian_settings;
