-- Game Space data exposure (issue #384, implementing #381's decided
-- policy): schema-level default visibility plus a bidirectional
-- field-level override, and the real instance-data publication mechanism
-- (`game_data.published`) the policy governs.
--
-- Both new `game_schemas`/`indexer_game_schemas` columns default to
-- today's implicit behavior (fully open, no restriction) so every schema
-- published before this migration keeps behaving exactly as it does
-- today — see `crates/server/src/game_schemas.rs`.
ALTER TABLE game_schemas
    ADD COLUMN default_visibility TEXT NOT NULL DEFAULT 'public'
        CHECK (default_visibility IN ('public', 'private')),
    ADD COLUMN field_visibility JSONB NOT NULL DEFAULT '{}'::jsonb;

ALTER TABLE indexer_game_schemas
    ADD COLUMN default_visibility TEXT NOT NULL DEFAULT 'public'
        CHECK (default_visibility IN ('public', 'private')),
    ADD COLUMN field_visibility JSONB NOT NULL DEFAULT '{}'::jsonb;

-- `game_data_instances` / `indexer_game_data_instances` mirror the
-- `game_schemas` / `indexer_game_schemas` pairing exactly: append-only,
-- `superseded_by` lineage, no PATCH — publishing a new instance for the
-- same `(schema_id, subject)` pair is a new row referencing the
-- superseded one, never an in-place update (`crates/server/src/game_data.rs`).
--
-- `instance` is stored opaque JSONB — validated against the schema's
-- parsed protobuf descriptor at publish time
-- (`protobuf-json-mapping`), but the validated value itself is stored
-- and served exactly as submitted, not re-encoded into some new wire
-- format (issue #384's amendment).
CREATE TABLE game_data_instances (
    id TEXT PRIMARY KEY,
    schema_id TEXT NOT NULL REFERENCES game_schemas(id) ON DELETE CASCADE,
    game_id UUID NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    subject UUID NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    instance JSONB NOT NULL,
    published_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    superseded_by TEXT,
    UNIQUE (schema_id, subject, id)
);

-- "The current instance for (schema, subject)" and "every instance
-- belonging to this subject" (the read endpoint's own lookup) via these
-- two prefixes.
CREATE INDEX game_data_instances_schema_subject_idx
    ON game_data_instances (schema_id, subject);
CREATE INDEX game_data_instances_subject_idx ON game_data_instances (subject);

CREATE TABLE indexer_game_data_instances (
    id TEXT PRIMARY KEY,
    schema_id TEXT NOT NULL,
    game_id UUID NOT NULL,
    subject UUID NOT NULL,
    instance JSONB NOT NULL,
    published_at TIMESTAMPTZ NOT NULL,
    superseded_by TEXT
);

CREATE INDEX indexer_game_data_instances_schema_subject_idx
    ON indexer_game_data_instances (schema_id, subject);
CREATE INDEX indexer_game_data_instances_subject_idx
    ON indexer_game_data_instances (subject);
