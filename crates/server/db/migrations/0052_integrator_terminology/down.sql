-- Exact inverse of up.sql (issue #290) — every statement is a rename, so
-- reverting is lossless.

ALTER INDEX indexer_integrator_data_instances_subject_idx
    RENAME TO indexer_game_data_instances_subject_idx;
ALTER INDEX indexer_integrator_data_instances_schema_subject_idx
    RENAME TO indexer_game_data_instances_schema_subject_idx;
ALTER INDEX integrator_data_instances_subject_idx RENAME TO game_data_instances_subject_idx;
ALTER INDEX integrator_data_instances_schema_subject_idx
    RENAME TO game_data_instances_schema_subject_idx;
ALTER INDEX indexer_integrator_bindings_integrator_id_idx
    RENAME TO indexer_game_bindings_game_id_idx;
ALTER INDEX indexer_integrator_schemas_integrator_id_idx
    RENAME TO indexer_game_schemas_game_id_idx;
ALTER INDEX integrator_schemas_integrator_id_idx RENAME TO game_schemas_game_id_idx;
ALTER INDEX achievement_attestations_integrator_id_idx
    RENAME TO achievement_attestations_game_id_idx;
ALTER INDEX issuer_keys_integrator_id_active_idx RENAME TO issuer_keys_game_id_active_idx;
ALTER INDEX bindings_one_active_per_identity_integrator
    RENAME TO bindings_one_active_per_identity_game;

ALTER TABLE presence_preferences RENAME COLUMN hide_active_in TO hide_playing;
ALTER TABLE integrators RENAME COLUMN owner_name TO developer;

ALTER TABLE indexer_integrator_data_instances RENAME COLUMN integrator_id TO game_id;
ALTER TABLE integrator_data_instances RENAME COLUMN integrator_id TO game_id;
ALTER TABLE indexer_integrator_bindings RENAME COLUMN integrator_id TO game_id;
ALTER TABLE indexer_integrator_schemas RENAME COLUMN integrator_id TO game_id;
ALTER TABLE integrator_schemas RENAME COLUMN integrator_id TO game_id;
ALTER TABLE guild_favorite_games RENAME COLUMN integrator_id TO game_id;
ALTER TABLE guild_integrator_associations RENAME COLUMN integrator_id TO game_id;
ALTER TABLE achievement_attestations RENAME COLUMN integrator_id TO game_id;
ALTER TABLE achievement_definitions RENAME COLUMN integrator_id TO game_id;
ALTER TABLE bindings RENAME COLUMN integrator_id TO game_id;
ALTER TABLE issuer_keys RENAME COLUMN integrator_id TO game_id;
ALTER TABLE integrator_challenges RENAME COLUMN integrator_id TO game_id;
ALTER TABLE integrator_requested_capabilities RENAME COLUMN integrator_id TO game_id;

ALTER TABLE indexer_integrator_data_instances RENAME TO indexer_game_data_instances;
ALTER TABLE integrator_data_instances RENAME TO game_data_instances;
ALTER TABLE indexer_integrator_bindings RENAME TO indexer_game_bindings;
ALTER TABLE indexer_integrator_schemas RENAME TO indexer_game_schemas;
ALTER TABLE integrator_schemas RENAME TO game_schemas;
ALTER TABLE guild_integrator_associations RENAME TO guild_game_associations;
ALTER TABLE integrator_challenges RENAME TO game_challenges;
ALTER TABLE integrator_requested_capabilities RENAME TO game_requested_capabilities;
ALTER TABLE integrators RENAME TO games;
