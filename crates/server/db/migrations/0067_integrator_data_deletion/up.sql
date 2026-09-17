-- Issue #533: append-only tombstone for a published Integrator Space
-- instance (e.g. a deleted character), following `docs/architecture/revocation.md`'s
-- pattern — never a physical delete/mutation of the original row.
-- `game_data.deleted` sets these columns; the original `instance` JSONB and
-- `published_at` are never touched. `integrator_data_instances` is the
-- authoring-node write table, `indexer_integrator_data_instances` is the
-- mirror-reconstructible projection (same pairing every other table in
-- this module already has) — both get the same columns so either can
-- answer "is this instance currently active" without a join.
ALTER TABLE integrator_data_instances
    ADD COLUMN deleted_at TIMESTAMPTZ,
    ADD COLUMN delete_reason_code TEXT,
    ADD COLUMN delete_reason TEXT;

ALTER TABLE indexer_integrator_data_instances
    ADD COLUMN deleted_at TIMESTAMPTZ,
    ADD COLUMN delete_reason_code TEXT,
    ADD COLUMN delete_reason TEXT;
