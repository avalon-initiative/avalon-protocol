DROP TABLE indexer_game_data_instances;
DROP TABLE game_data_instances;

ALTER TABLE indexer_game_schemas
    DROP COLUMN default_visibility,
    DROP COLUMN field_visibility;

ALTER TABLE game_schemas
    DROP COLUMN default_visibility,
    DROP COLUMN field_visibility;
