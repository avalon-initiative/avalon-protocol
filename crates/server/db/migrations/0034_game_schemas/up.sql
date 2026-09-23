-- Game Space schema publication, closing issue #255 (decided by #181).
--
-- `game_schemas` is a rebuildable projection of `game_schema.published`
-- (`crates/server/src/game_schemas.rs`), same posture as every other
-- projection in this repo: the outbox-written event is canonical, this row
-- is a cache of it.
--
-- `id` is the version's namespaced `GlobalId`
-- (`game:<slug>:schema:<version>`, `crates/protocol/src/ids.rs`) stored as
-- its wire string, same choice `achievement_definitions.id` already made.
-- `UNIQUE (game_id, version)` is an explicit separate constraint for the
-- same reason that table keeps `UNIQUE (game_id, key)` alongside `id`.
--
-- `proto_source` is never updated once inserted — evolving a schema means
-- inserting a new row with `version` one higher, never rewriting this one.
-- The only column ever touched on an existing row is `superseded_by`, set
-- once when the next version is published (see the module doc comment on
-- `crates/protocol/src/game_schemas.rs` for why that specific exception to
-- immutability is fine: it is lineage metadata, not the published text).
CREATE TABLE game_schemas (
    id TEXT PRIMARY KEY,
    game_id UUID NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    version INT NOT NULL,
    proto_source TEXT NOT NULL,
    published_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    superseded_by TEXT,
    UNIQUE (game_id, version)
);

-- "List every version a game has published" (GET /games/{slug}/schemas,
-- and the registry read-model projection below) via this prefix.
CREATE INDEX game_schemas_game_id_idx ON game_schemas (game_id);

-- The indexer's own read model (`crates/indexer/src/projections/game_schemas.rs`),
-- kept as its own table per this repo's settlement-vs-querying split
-- (`docs/architecture/query-and-indexing.md`) rather than the server
-- querying `game_schemas` above directly for registry discovery — the
-- server's table is this endpoint's own request-serving cache; the
-- indexer's is the durable-event-derived read model the registry surfaces,
-- and the two are allowed to diverge in implementation even though they
-- hold the same facts today.
CREATE TABLE indexer_game_schemas (
    id TEXT PRIMARY KEY,
    game_id UUID NOT NULL,
    version INT NOT NULL,
    proto_source TEXT NOT NULL,
    published_at TIMESTAMPTZ NOT NULL,
    superseded_by TEXT,
    UNIQUE (game_id, version)
);

CREATE INDEX indexer_game_schemas_game_id_idx ON indexer_game_schemas (game_id);
