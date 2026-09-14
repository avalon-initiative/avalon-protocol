-- Generalize the schema's gaming vocabulary to the integrator vocabulary the
-- code already models (issue #290). Gaming is Avalon's first integrator
-- category, not its only one — `IntegratorCategory::{Game,App,Service}`
-- (#282) shipped that idea; this brings the tables underneath it in line.
--
-- Pure renames: no table is created or dropped, no column changes type, and
-- every row keeps its data. Postgres rewrites dependent foreign keys, unique
-- constraints and index predicates automatically on a column rename, so only
-- the index *names* need restating (purely cosmetic, but left matching their
-- tables so a future reader isn't hunting for `game_*` objects).
--
-- Deliberately NOT renamed here, and not by accident:
--   * `guild_favorite_games` and `guilds.game_breakdown_public` — a guild's
--     favorite *games* and its "what do our members play" breakdown are
--     gaming's own product vocabulary, not the generic integrator primitive.
--     Their `game_id` columns still become `integrator_id`, since that column
--     is the generic handle to a registered integrator either way.
--   * `indexer_integrator_bindings.players` / `total_players_ever` — the
--     Integrator Registry's player-count metric is genuinely about a specific
--     game's players, a separate question from the generic identity handle.
--   * Ledger event kinds (`game.registered`, `game.binding_established`,
--     `game.binding_ended`, `game_schema.published`, `game_data.published`)
--     and their payload keys, which live in `ledger_entries` rows this
--     migration never touches — they are hash-chained protocol history and
--     the indexer replays them verbatim.

ALTER TABLE games RENAME TO integrators;
ALTER TABLE game_requested_capabilities RENAME TO integrator_requested_capabilities;
ALTER TABLE game_challenges RENAME TO integrator_challenges;
ALTER TABLE guild_game_associations RENAME TO guild_integrator_associations;
ALTER TABLE game_schemas RENAME TO integrator_schemas;
ALTER TABLE indexer_game_schemas RENAME TO indexer_integrator_schemas;
ALTER TABLE indexer_game_bindings RENAME TO indexer_integrator_bindings;
ALTER TABLE game_data_instances RENAME TO integrator_data_instances;
ALTER TABLE indexer_game_data_instances RENAME TO indexer_integrator_data_instances;

-- `game_id` is the generic "which registered integrator" handle everywhere it
-- appears, including on the two tables whose own names stay gaming-flavored.
ALTER TABLE integrator_requested_capabilities RENAME COLUMN game_id TO integrator_id;
ALTER TABLE integrator_challenges RENAME COLUMN game_id TO integrator_id;
ALTER TABLE issuer_keys RENAME COLUMN game_id TO integrator_id;
ALTER TABLE bindings RENAME COLUMN game_id TO integrator_id;
ALTER TABLE achievement_definitions RENAME COLUMN game_id TO integrator_id;
ALTER TABLE achievement_attestations RENAME COLUMN game_id TO integrator_id;
ALTER TABLE guild_integrator_associations RENAME COLUMN game_id TO integrator_id;
ALTER TABLE guild_favorite_games RENAME COLUMN game_id TO integrator_id;
ALTER TABLE integrator_schemas RENAME COLUMN game_id TO integrator_id;
ALTER TABLE indexer_integrator_schemas RENAME COLUMN game_id TO integrator_id;
ALTER TABLE indexer_integrator_bindings RENAME COLUMN game_id TO integrator_id;
ALTER TABLE integrator_data_instances RENAME COLUMN game_id TO integrator_id;
ALTER TABLE indexer_integrator_data_instances RENAME COLUMN game_id TO integrator_id;

-- "Who owns/operates this integrator." `owner_name` rather than `operator`,
-- which this codebase already uses for settlement/node operators.
ALTER TABLE integrators RENAME COLUMN developer TO owner_name;

-- Presence: "playing" only reads correctly for a game. `active_in` is the
-- same field for a game, an app or a service.
ALTER TABLE presence_preferences RENAME COLUMN hide_playing TO hide_active_in;

ALTER INDEX bindings_one_active_per_identity_game
    RENAME TO bindings_one_active_per_identity_integrator;
ALTER INDEX issuer_keys_game_id_active_idx RENAME TO issuer_keys_integrator_id_active_idx;
ALTER INDEX achievement_attestations_game_id_idx
    RENAME TO achievement_attestations_integrator_id_idx;
ALTER INDEX game_schemas_game_id_idx RENAME TO integrator_schemas_integrator_id_idx;
ALTER INDEX indexer_game_schemas_game_id_idx
    RENAME TO indexer_integrator_schemas_integrator_id_idx;
ALTER INDEX indexer_game_bindings_game_id_idx
    RENAME TO indexer_integrator_bindings_integrator_id_idx;
ALTER INDEX game_data_instances_schema_subject_idx
    RENAME TO integrator_data_instances_schema_subject_idx;
ALTER INDEX game_data_instances_subject_idx RENAME TO integrator_data_instances_subject_idx;
ALTER INDEX indexer_game_data_instances_schema_subject_idx
    RENAME TO indexer_integrator_data_instances_schema_subject_idx;
ALTER INDEX indexer_game_data_instances_subject_idx
    RENAME TO indexer_integrator_data_instances_subject_idx;
