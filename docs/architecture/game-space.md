# Game Space: Schemas, Publication, and Mapping

**A binding says an identity participates in a game. A Game Space says what the
game has chosen to describe to Avalon about its own data model, and how much
of it is actually exposed.** Neither one gives Avalon an opinion about what a
"level" or a "class" is. This document is the design for the layer that lets a
game make its data *structurally describable, versioned, discoverable, and
attributable to itself* without Avalon standardizing what that data means.

**Schema publication is now built; mapping and data exposure are not.**
#181 decided the representation and #255 built the first implementation
slice — a game can publish a versioned, immutable `.proto` description of
its own data model and have that publication discoverable through the Game
Registry. Mapping (a documented relationship between two schema versions)
and data exposure (actual instances) remain unbuilt — see
[Today in the repo](#today-in-the-repo).

## Game Space is not a new participation record

[Game bindings](./game-bindings.md) already establish "this identity
participates in this game," deliberately with no game-data field, per
[#67](https://github.com/LunarVagabond/avalon-protocol/issues/67). A **Game
Space** is a different thing: it belongs to the *game*, not to an
identity-game pair, and it is where a game optionally publishes *how its data is
shaped* (schemas) and optionally exposes *instances of that data* (data
exposure). It does not replace the game's own database, and it does not
replace or extend `GameBinding`:

```text
Game
  │
  ├── (game-side) full internal model — combat, quests, world state,
  │                matchmaking, complete character state. Avalon never
  │                sees this by default.
  │
  └── Game Space (Avalon-facing, optional, incremental)
        ├── declared schemas          "here is how our data is structured"
        ├── schema versions           "here is how that structure evolved"
        ├── exposed entities          "here are instances we choose to expose"
        └── mappings between versions "here is how v1 corresponds to v2"
```

A game with zero Game Space content is still a fully valid Avalon integration
today — identity, friends, guilds, and achievements do not require one.

## Minimal vs. deep integration

Nothing about Game Space is required. A small/indie game can stay exactly
where achievements already put it:

```text
Minimal
  Character ID + name, achievements, selected assets
  → no declared schema at all; the achievement `schema` field
    (achievements-and-attestations.md) is already sufficient
```

A game that wants richer interoperability declares more:

```text
Deep
  character (identity, progression, skills, reputation, equipment),
  items, achievements, titles — each a declared, versioned schema
  → enables migration, portability, cross-game recognition,
    developer-to-developer integration
```

Deep integration is opt-in per entity, the same way [portable assets are
opt-in per item](./future-layers.md#portable-assets-phase-4). Avalon never
requires a game to expose its full model to participate.

## Schema vs. data exposure — do not conflate these

**Built as of #384** (decided by #381): schema publication and instance-data
publication are two separate write paths with independent authorization, and
data exposure defaults to network-readable the same way attestations already
do — publishing is the opt-in. See "Today in the repo" below for the actual
mechanism.

```text
Schema publication  =  "Here is how our data is structured."
Data exposure        =  "Here are the instances we choose to expose."
```

A game can publish a `character/v3` schema publicly (so other developers and
the registry can read its shape) without exposing a single actual character.
Conversely nothing should let data exposure imply schema publication, or vice
versa — they are independent decisions with independent authorization.

This mirrors [provenance vs. functionality](./future-layers.md#portable-assets-phase-4):
schema is about shape, exposure is about instances, and neither one is about
meaning — a game's own `character.level` means whatever that game says it
means, same as an asset's function is whatever the issuing game says it does.

## Schema vs. mapping — a second distinction to hold

```text
Schema
  describes a data model
  game:ashen-realms/character/v1

Mapping
  describes a relationship between two models
  game:ashen-realms/character/v1 -> game:ashen-realms/character/v2
```

Not every schema needs a mapping, and not every mapping needs to be
mechanically executable — a mapping may just document field correspondence for
a human or another developer to read (renames, merges, splits, dropped
fields, default values), rather than being a transformation Avalon runs.
**The integrator owns the semantic transformation.** Avalon's job is
infrastructure for describing, storing, discovering, and validating a mapping
where one exists — never deciding what a game's migration means.

## Worked example

```json
// game:ashen-realms/character/v1
{ "level": "u32", "xp": "u64", "skills.fishing.level": "u32" }

// game:ashen-realms/character/v2
{ "progression.rank": "u32", "progression.experience": "u64",
  "professions.fishing": "u32" }

// mapping v1 -> v2
{ "level": "progression.rank",
  "xp": "progression.experience",
  "skills.fishing.level": "professions.fishing" }
```

A second game is free to model the same idea however it wants —
`character.power_rating`, `character.specializations[]`, whatever fits its
own game. Nothing above requires Game B to converge on Game A's field names.

An emitted data instance is not bare JSON. It is an instance of a declared,
versioned schema, attributable to its owning game:

```json
{
  "schema": "game:ashen-realms/character/v1",
  "entity": "character",
  "id": "character:12345",
  "data": { "level": 87, "xp": 18293821 }
}
```

JSON (or whatever wire format is eventually chosen) is the transport. The
schema reference is what makes the payload interpretable — the same relationship
`AchievementDefinition.schema` already has to an attestation's payload
([`./achievements-and-attestations.md`](./achievements-and-attestations.md)).
**Data exposure/mapping validation mechanism is still not decided by this
document** — only schema representation, storage, and versioning (below)
are — see [Decisions and tickets](#decisions-and-tickets).

## Representation and versioning (decided by #181, built by #255, parsing added by #384)

A published game schema is protobuf IDL (`.proto`) — mature
field-numbering/evolution rules, broad developer familiarity. It is stored,
versioned, and served back verbatim, exactly as submitted (`proto_source`
itself is never rewritten or re-encoded). #181 originally specified this
text as opaque to Avalon; #384's amendment revised that — Avalon now
actually parses it (pure-Rust, no `protoc` binary) to resolve its root
message, both to validate `field_visibility`'s field names for real and to
validate submitted instance data against it. This keeps the actual network
surface JSON-only (per [#82](https://github.com/LunarVagabond/avalon-protocol/issues/82)'s
protocol-event-payload policy): protobuf's canonical JSON mapping
(`protobuf-json-mapping`) is the bridge #384 actually uses, not a reason to
make Avalon's API gRPC or binary-protobuf-on-the-wire.

Version identity is a monotonic `version: u32` per game — the same
precedent `AchievementDefinition.version` already established — plus an
`superseded_by: Option<GlobalId>` pointer set on a version once a later one
supersedes it, so lineage is traceable without ever rewriting a published
version's `.proto` text. A version, once published, is immutable: evolving
a schema means publishing a new version, never editing one in place. See
`avalon_protocol::game_schemas::GameSchemaVersion`
(`crates/protocol/src/game_schemas.rs`) and
`crates/server/src/game_schemas.rs` (`POST`/`GET
/games/{slug}/schemas[/{version}]`).

Still not decided or built: server-side validation of exposed data against
a published schema, and mapping between schema versions — both explicitly
deferred to their own follow-up tickets under Epic
[#182](https://github.com/LunarVagabond/avalon-protocol/issues/182).

## Historical interpretation

A schema version is immutable once data has been recorded against it. Old data
is read according to the schema version it was created under; a new schema
version never silently reinterprets it. This follows the same rule
[protocol events already use](./protocol-events.md#versioning-policy): every
version stays decodable forever, decoders are added not replaced, and a field
rename or meaning change is a new version, not an edit to an old one. A schema
publication system would reuse that policy rather than invent a second one.

## Universal vs. game-specific semantics — the capability-mapping question

A game's `character.level` field does not make "level" an Avalon-wide concept.
Avalon may eventually define optional interoperable capabilities that a game
can *choose* to map its own fields onto:

```text
Protocol primitive: character.progression.level   (optional, Avalon-defined)
        │
        ├── Game A maps: character.level → character.progression.level
        └── Game B maps: character.rank  → character.progression.tier
```

This is explicitly **not** decided or scoped by this document, and must not be
built ahead of real demand — see
[Proposal §32](../stakeholders/Proposal.md#32-open-questions), which already
lists "should assets have standardized schemas?" as open. The architecture
should allow this to exist later (a schema field can reference an optional
capability mapping) without requiring anyone to design a universal
progression/skills/class taxonomy now.

## Provenance

Game Space data reuses the existing [provenance](./provenance.md) model
rather than inventing a parallel one: an exposed entity has an owning game
(source), a schema/version it was recorded under, and — where a mapping was
used to produce it — the mapping and the source schema/version it came from,
exactly as an asset's issuance and transfer history already work
([`./future-layers.md`](./future-layers.md)). No new provenance primitive is
required; a schema/mapping reference on an event or attestation is enough.

## Registry discoverability, not settlement bulk

Publication *metadata* (schema id, version, owner, lineage) is the kind of
durable-derived fact the [Game Registry](./game-registry.md) already exists
to surface — the same place capabilities, keys, and activity metrics live
today. #255 built exactly this: `crates/indexer/src/projections/game_schemas.rs`
projects `game_schema.published` events into the registry's read model,
reusing the same decode/apply projection shape every other read model in
that crate already uses, rather than a separate discovery path. Full schema
bodies and every data instance are not settlement-layer material by default;
the registry/indexer is the query surface, and the settlement layer commits
selectively (a schema's hash or id, not its full body) only where a durable
guarantee is actually needed, following the same discipline
[`./settlement.md`](./settlement.md) and
[`./future-layers.md`](./future-layers.md) already apply to assets.

## What this explicitly does not do

- Does not require any game to expose its internal model beyond what it
  chooses.
- Does not make Avalon the arbiter of what a game's fields mean.
- Does not imply that using Avalon makes a character portable — portability of
  any kind stays opt-in and per-entity, per
  [`./future-layers.md`](./future-layers.md).
- Does validate exposed data against a published schema, and does parse
  `.proto` IDL to do so — see #384 in [Representation and
  versioning](#representation-and-versioning-decided-by-181-built-by-255-parsing-added-by-384).
  Compiling to a language binding (codegen) is still out of scope; parsing
  here is only ever for structural validation.
- Does not extend `GameBinding` or the achievement `schema` field to carry
  general game data; both stay exactly as narrowly scoped as they are today.

## Today in the repo

- `avalon_protocol::game_schemas::GameSchemaVersion`
  (`crates/protocol/src/game_schemas.rs`) is the one domain type: game id,
  raw `.proto` source, monotonic `version: u32`, `published_at`, and an
  optional `superseded_by: Option<GlobalId>` lineage pointer — the
  `AchievementDefinition.schema`/`version` precedent
  ([`./achievements-and-attestations.md`](./achievements-and-attestations.md)),
  generalized from a bare reference field to the schema description itself.
  No `GameSpace`/`Mapping`/`Entity` type exists yet — mapping between schema
  versions remains unbuilt. Data exposure (this section's own gap) is now
  built, per #384.
- **`proto_source` is now actually parsed** (#384's amendment to #181's
  original "protobuf IDL, stored opaque" decision). `crate::proto_schema`
  (`crates/server/src/proto_schema.rs`) parses it at publish time with
  `protobuf-parse`'s pure-Rust parser — no `protoc` binary, since this parses
  untrusted third-party text at request time — into a `FileDescriptorProto`,
  then builds a `MessageDescriptor` via `protobuf`'s reflection support. A
  schema must declare **exactly one top-level `message`**, which becomes the
  schema's root type; zero or multiple is rejected with
  `AppError::InvalidProtoSchema`, never guessed at. The *validated* instance
  JSON is still stored/served as plain JSONB afterward — the protobuf
  machinery is a write-time validation gate, not a new storage/wire format.
- **Schema-level and field-level visibility** (`default_visibility`:
  `"public"`/`"private"`, `field_visibility`: field name ->
  `"public"`/`"private"`, overriding the default for that field in either
  direction) live on `game_schemas`/`indexer_game_schemas`
  (`crates/server/db/migrations/0051_game_data_visibility`). Both default
  to fully open (`"public"`, `{}`) — a schema published before #384 landed
  keeps behaving exactly as it did before. `field_visibility`'s keys are
  validated against the parsed root message's real field names at publish
  time (`proto_schema::validate_field_visibility_keys`); a nonexistent
  field name is rejected, not silently accepted.
- **Instance-data publication**: `POST
  /games/{slug}/schemas/{version}/data` (`crates/server/src/game_data.rs`) —
  a new `game_data.published` event kind, with `game_data_instances` +
  `indexer_game_data_instances` tables mirroring the
  `game_schemas`/`indexer_game_schemas` pairing exactly (append-only,
  `superseded_by` lineage, no PATCH).

  Write auth needs three things together: `authenticate_owning_game` (the
  caller must *be* `{slug}`, same guard `publish_schema_version` uses); an
  active binding from the subject identity to that game
  (`authz::has_active_binding`, mirroring `achievements::issue_attestation`'s
  "the player's own consent" pattern); and the resolved schema's own
  `game_id` matching the caller. The submitted `instance` JSON is then
  validated against the schema's parsed root message via
  `protobuf-json-mapping` (`proto_schema::validate_instance_json`) — unknown
  fields, wrong types, and (for a proto2-style schema) missing `required`
  fields are all rejected with `AppError::InstanceSchemaMismatch`, never
  stored.
- **Read**: `GET /identities/{id}/game-data`
  (`game_data::get_identity_game_data`) — public, unauthenticated, same
  posture `GET /attestations/{id}` already has (#381's whole point).
  Reads the indexer's own projection
  (`avalon_indexer::projections::game_data_instances`), never raw
  ledger/outbox data, and applies the bidirectional visibility rule
  (`game_data::resolve_visible_fields`, a pure function unit-tested
  directly) per instance: a field is included iff `default_visibility` is
  `"public"` and the field isn't marked `"private"`, or `default_visibility`
  is `"private"` and the field is marked `"public"`. Only literal top-level
  JSON key matching — no nested-field visibility in this pass (documented
  limitation, not silently attempted).
- **In-process parse cache.** `proto_schema` caches each schema's parsed
  root message (keyed by schema id, which is permanently immutable once
  published) so repeated instance-data writes against the same schema
  version don't re-run the `.proto` parser on every request — parsing
  happens once at publish time (`proto_schema::cache_root_message`, called
  right after a successful `publish_schema_version`) and, thereafter, at
  most once per server process per schema (`parse_root_message_cached`).
- `GlobalId::new(namespace, owner, kind, key)`
  (`crates/protocol/src/ids.rs`) namespaces a version as
  `game:<slug>:schema:<version>`, minted by
  `crates/server/src/game_schemas.rs::schema_ref`.
- `crates/server/src/game_schemas.rs` — `POST /games/{slug}/schemas`
  (publish the next version, game-credential-authenticated the same way
  `achievements.rs` authenticates achievement-definition writes — proving
  the game owns the slug, not an identity-granted capability), `GET
  /games/{slug}/schemas` (list, public), `GET
  /games/{slug}/schemas/{version}` (one version, public). `game_schemas`
  (`crates/server/db/migrations/0034_game_schemas`) is the request-serving
  projection; `proto_source` is never updated once inserted.
- The [Game Registry](./game-registry.md)'s read model now covers schema
  discovery: `crates/indexer/src/projections/game_schemas.rs` projects
  `game_schema.published` into `indexer_game_schemas`, queried by
  `list_for_game`.
- [#82](https://github.com/LunarVagabond/avalon-protocol/issues/82) is a
  versioning policy for `ProtocolEvent.kind`/payload, i.e. Avalon's own
  events — not a mechanism for versioning a game's data model. Schema
  versioning follows the same discipline without #82 itself being widened
  to cover it.
- Mapping between schema versions is still unbuilt — see #182's remaining
  tickets. Server-side validation of exposed data against a schema is now
  built (#384, above).

## Decisions and tickets

- [Proposal §32](../stakeholders/Proposal.md#32-open-questions) — "should
  assets have standardized schemas?" is the existing, still-open question this
  document generalizes from assets to game data broadly.
- [#181](https://github.com/LunarVagabond/avalon-protocol/issues/181) —
  Decision: game-defined schema model, representation, and versioning
  strategy. Decided 2026-09-09: protobuf IDL as the description format,
  originally stored opaque; #384's amendment revised this to real parsing —
  see below.
- [#255](https://github.com/LunarVagabond/avalon-protocol/issues/255) —
  Game Schema Publication: the first implementation ticket under #182,
  building publication, immutability/lineage, and registry discovery per
  #181's decision.
- [#381](https://github.com/LunarVagabond/avalon-protocol/issues/381) —
  Decision: data exposure defaults to network-readable once an integrator
  publishes instance data against its own published schema, with a
  schema-level opt-out and a bidirectional field-level override.
- [#384](https://github.com/LunarVagabond/avalon-protocol/issues/384) —
  Game Space data exposure: builds #381's decision — schema visibility
  metadata, real instance-data publication (`game_data.published`), the
  visibility-enforcing read endpoint, and (its own amendment) real
  protobuf parsing/validation of both `proto_source` and submitted
  instances, replacing #181's original "stored opaque, never parsed"
  stance for schema text specifically.
- [#182](https://github.com/LunarVagabond/avalon-protocol/issues/182) — Epic:
  Game Space & Schema Publication, gated on #181.
- [#67](https://github.com/LunarVagabond/avalon-protocol/issues/67) — ADR:
  identity is separate from game characters; the reason Game Space does not
  become a second game database.
- [#83](https://github.com/LunarVagabond/avalon-protocol/issues/83) — game
  bindings; Game Space is explicitly not an extension of this.
- [#94](https://github.com/LunarVagabond/avalon-protocol/issues/94) — Epic:
  Game Registry & Network Intelligence; schema discovery lives in its read
  surface (`crates/indexer/src/projections/game_schemas.rs`), not a new
  service.
- [#82](https://github.com/LunarVagabond/avalon-protocol/issues/82) — protocol
  event kind/versioning policy; the pattern a schema versioning policy would
  follow, not extend.
- [#98](https://github.com/LunarVagabond/avalon-protocol/issues/98) — typed,
  enum-backed capability strings; the pattern a schema/entity-kind catalogue
  would reuse if one is ever built.
