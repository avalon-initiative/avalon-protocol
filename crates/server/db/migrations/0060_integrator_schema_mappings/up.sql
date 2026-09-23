-- Integrator Space schema-to-schema mapping model, closing #182's last
-- unfiled acceptance item (#491). Not an execution engine: a mapping
-- documents a correspondence between two of an integrator's own already-
-- published schema versions (renames, merges, splits, dropped fields,
-- default values) for a human or another developer to read — Avalon
-- never runs the transformation it describes.
--
-- `id` is the mapping's namespaced `GlobalId`
-- (`game:<slug>:schema_mapping:<seq>`, `crates/protocol/src/ids.rs`)
-- stored as its wire string, same choice `integrator_schemas.id` already
-- made. `from_schema_id`/`to_schema_id` are the two schema versions'
-- own `GlobalId` wire strings (`game:<slug>:schema:<version>`) — plain
-- TEXT, not a foreign key, matching `integrator_schemas.superseded_by`'s
-- own precedent of validating existence/ownership at write time in the
-- handler rather than a DB-enforced FK.
--
-- Immutable once published — there is no `superseded_by`/lineage concept
-- here the way schema versions have one: a mapping doesn't supersede a
-- prior mapping, it just documents one (from, to) correspondence. An
-- integrator that wants to correct a mapping publishes a new row; the old
-- one stays discoverable.
--
-- `field_correspondence` is a simple old-field -> new-field JSONB map
-- (renames), mirroring `integrator_schemas.field_visibility`'s own
-- shape; `description` is free text for whatever a flat map can't
-- capture (merges, splits, dropped fields, default values) — see
-- `docs/architecture/integrator-space.md`'s worked example.
CREATE TABLE integrator_schema_mappings (
    id TEXT PRIMARY KEY,
    integrator_id UUID NOT NULL REFERENCES integrators(id) ON DELETE CASCADE,
    from_schema_id TEXT NOT NULL,
    to_schema_id TEXT NOT NULL,
    description TEXT NOT NULL,
    field_correspondence JSONB NOT NULL DEFAULT '{}'::jsonb,
    published_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- "List every mapping an integrator has published" (GET
-- /integrations/{slug}/mappings, and the registry read-model projection
-- below) via this prefix.
CREATE INDEX integrator_schema_mappings_integrator_id_idx
    ON integrator_schema_mappings (integrator_id);

-- The indexer's own read model
-- (`crates/indexer/src/projections/integrator_schema_mappings.rs`), kept as
-- its own table per this repo's settlement-vs-querying split
-- (`docs/architecture/query-and-indexing.md`) — same posture
-- `indexer_integrator_schemas` already established for schema versions.
CREATE TABLE indexer_integrator_schema_mappings (
    id TEXT PRIMARY KEY,
    integrator_id UUID NOT NULL,
    from_schema_id TEXT NOT NULL,
    to_schema_id TEXT NOT NULL,
    description TEXT NOT NULL,
    field_correspondence JSONB NOT NULL DEFAULT '{}'::jsonb,
    published_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX indexer_integrator_schema_mappings_integrator_id_idx
    ON indexer_integrator_schema_mappings (integrator_id);
