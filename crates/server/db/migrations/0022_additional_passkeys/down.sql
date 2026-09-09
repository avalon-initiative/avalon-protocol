-- Reverts 0022_additional_passkeys/up.sql. Assumes no 'add_passkey' rows
-- remain (ceremony rows are short-lived by design, TTL'd within minutes),
-- same assumption every other down migration in this repo makes about the
-- ephemeral state it's narrowing back down.
ALTER TABLE webauthn_ceremonies DROP CONSTRAINT webauthn_ceremonies_kind_check;
ALTER TABLE webauthn_ceremonies
    ADD CONSTRAINT webauthn_ceremonies_kind_check
    CHECK (kind IN ('registration', 'authentication'));
