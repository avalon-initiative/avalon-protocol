-- Issue #543: a second, independent axis from `role` (who can change the
-- key set) -- what the key is authorized to sign. `attestation` (the
-- default, matching what every pre-#543 key already meant) or
-- `shard_settlement` (#527/#529's sharded-settlement key domain,
-- authorized through this exact same issuer-key registration flow rather
-- than a second, separate registry -- see
-- docs/architecture/network-trust-anchors.md's "Per-shard trust anchors"
-- section).
ALTER TABLE issuer_keys
    ADD COLUMN purpose TEXT NOT NULL DEFAULT 'attestation';
