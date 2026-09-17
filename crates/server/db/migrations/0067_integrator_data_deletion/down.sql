ALTER TABLE indexer_integrator_data_instances
    DROP COLUMN deleted_at,
    DROP COLUMN delete_reason_code,
    DROP COLUMN delete_reason;

ALTER TABLE integrator_data_instances
    DROP COLUMN deleted_at,
    DROP COLUMN delete_reason_code,
    DROP COLUMN delete_reason;
