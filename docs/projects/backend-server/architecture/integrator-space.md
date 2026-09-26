# Integrator Space: Schemas, Publication, and Mapping

**A binding says an identity participates in an integrator. An Integrator Space says what the
integrator has chosen to describe to Avalon about its own data model, and how much
of it is actually exposed.** Neither one gives Avalon an opinion about what a
"level" or a "class" is. This document is the design for the layer that lets a
integrator make its data *structurally describable, versioned, discoverable, and
attributable to itself* without Avalon standardizing what that data means.

**Schema publication, data exposure, and mapping are all built.** An integrator can
publish a versioned, immutable `.proto` description of its own data model and have
that publication discoverable through the Integrator Registry. Data exposure (actual
instances, with visibility control) and mapping — a documented relationship between
two schema versions, explicitly not an execution engine — are both built too. See
[Current implementation](#current-implementation).

## Integrator Space is not a new participation record

[Integrator bindings](./bindings.md) already establish "this identity
participates in this integrator," deliberately with no integrator-data field.
A **Integrator Space** is a different thing: it belongs to the *integrator*, not to an
identity-integrator pair, and it is where an integrator optionally publishes *how its data is
shaped* (schemas) and optionally exposes *instances of that data* (data
exposure). It does not replace the integrator's own database, and it does not
replace or extend `IntegratorBinding`:

```text
Integrator
  │
  ├── (integrator-side) full internal model — combat, quests, world state,
  │                matchmaking, complete character state. Avalon never
  │                sees this by default.
  │
  └── Integrator Space (Avalon-facing, optional, incremental)
        ├── declared schemas          "here is how our data is structured"
        ├── schema versions           "here is how that structure evolved"
        ├── exposed entities          "here are instances we choose to expose"
        └── mappings between versions "here is how v1 corresponds to v2"
```

An integrator with zero Integrator Space content is still a fully valid Avalon integration
today — identity, friends, guilds, and achievements do not require one.

## Minimal vs. deep integration

Nothing about Integrator Space is required. A small/indie integrator can stay exactly
where achievements already put it:

```text
Minimal
  Character ID + name, achievements, selected assets
  → no declared schema at all; the achievement `schema` field
    (achievements-and-attestations.md) is already sufficient
```

An integrator that wants richer interoperability declares more:

```text
Deep
  character (identity, progression, skills, reputation, equipment),
  items, achievements, titles — each a declared, versioned schema
  → enables migration, portability, cross-integrator recognition,
    developer-to-developer integration
```

Deep integration is opt-in per entity, the same way [portable assets are
opt-in per item](./future-layers.md#portable-assets-phase-4). Avalon never
requires an integrator to expose its full model to participate.

## Schema vs. data exposure — do not conflate these

Schema publication and instance-data publication are two separate write paths
with independent authorization, and data exposure defaults to network-readable
the same way attestations already do — publishing is the opt-in. See
[Current implementation](#current-implementation) for the actual mechanism.

```text
Schema publication  =  "Here is how our data is structured."
Data exposure        =  "Here are the instances we choose to expose."
```

An integrator can publish a `character/v3` schema publicly (so other developers and
the registry can read its shape) without exposing a single actual character.
Conversely nothing should let data exposure imply schema publication, or vice
versa — they are independent decisions with independent authorization.

This mirrors [provenance vs. functionality](./future-layers.md#portable-assets-phase-4):
schema is about shape, exposure is about instances, and neither one is about
meaning — an integrator's own `character.level` means whatever that integrator says it
means, same as an asset's function is whatever the issuing integrator says it does.

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
where one exists — never deciding what an integrator's migration means.

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

A second integrator is free to model the same idea however it wants —
`character.power_rating`, `character.specializations[]`, whatever fits its
own integrator. Nothing above requires Integrator B to converge on Integrator A's field names.

An emitted data instance is not bare JSON. It is an instance of a declared,
versioned schema, attributable to its owning integrator:

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
Data exposure and mapping validation are both built — see
[Current implementation](#current-implementation). Mapping *validation* stops
at structural sanity (both referenced schema versions exist and belong to the
publishing integrator) — never semantic correctness of the correspondence
itself, per this section's own "the integrator owns the semantic
transformation" invariant.

## Representation and versioning

A published integrator schema is protobuf IDL (`.proto`) — mature
field-numbering/evolution rules, broad developer familiarity. It is stored,
versioned, and served back verbatim, exactly as submitted (`proto_source`
itself is never rewritten or re-encoded). Avalon actually parses it
(pure-Rust, no `protoc` binary) to resolve its root message, both to validate
`field_visibility`'s field names for real and to validate submitted instance
data against it. This keeps the actual network surface JSON-only: protobuf's
canonical JSON mapping (`protobuf-json-mapping`) is the bridge actually used,
not a reason to make Avalon's API gRPC or binary-protobuf-on-the-wire.

Version identity is a monotonic `version: u32` per integrator — the same
precedent `AchievementDefinition.version` already established — plus an
`superseded_by: Option<GlobalId>` pointer set on a version once a later one
supersedes it, so lineage is traceable without ever rewriting a published
version's `.proto` text. A version, once published, is immutable: evolving
a schema means publishing a new version, never editing one in place. See
`avalon_protocol::integrator_schemas::IntegratorSchemaVersion`
(`crates/protocol/src/integrator_schemas.rs`) and
`crates/server/src/integrator_schemas.rs` (`POST`/`GET
/integrations/{slug}/schemas[/{version}]`).

Not yet built: server-side capability-mapping between universal Avalon-defined
concepts and an integrator's own fields — see below.

## Historical interpretation

A schema version is immutable once data has been recorded against it. Old data
is read according to the schema version it was created under; a new schema
version never silently reinterprets it. This follows the same rule
[protocol events already use](./protocol-events.md#versioning-policy): every
version stays decodable forever, decoders are added not replaced, and a field
rename or meaning change is a new version, not an edit to an old one. A schema
publication system would reuse that policy rather than invent a second one.

## Universal vs. game-specific semantics — the capability-mapping question

An integrator's `character.level` field does not make "level" an Avalon-wide concept.
Avalon may eventually define optional interoperable capabilities that an integrator
can *choose* to map its own fields onto:

```text
Protocol primitive: character.progression.level   (optional, Avalon-defined)
        │
        ├── Integrator A maps: character.level → character.progression.level
        └── Integrator B maps: character.rank  → character.progression.tier
```

This is explicitly **not** decided or scoped by this document, and must not be
built ahead of real demand — see
[Proposal §32](../../../stakeholders/Proposal.md#32-open-questions), which already
lists "should assets have standardized schemas?" as open. The architecture
should allow this to exist later (a schema field can reference an optional
capability mapping) without requiring anyone to design a universal
progression/skills/class taxonomy now.

## Provenance

Integrator Space data reuses the existing [provenance](./provenance.md) model
rather than inventing a parallel one: an exposed entity has an owning integrator
(source), a schema/version it was recorded under, and — where a mapping was
used to produce it — the mapping and the source schema/version it came from,
exactly as an asset's issuance and transfer history already work
([`./future-layers.md`](./future-layers.md)). No new provenance primitive is
required; a schema/mapping reference on an event or attestation is enough.

## Registry discoverability, not settlement bulk

Publication *metadata* (schema id, version, owner, lineage) is the kind of
durable-derived fact the [Integrator Registry](./registry.md) already exists
to surface — the same place capabilities, keys, and activity metrics live
today. `crates/indexer/src/projections/integrator_schemas.rs`
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

- Does not require any integrator to expose its internal model beyond what it
  chooses.
- Does not make Avalon the arbiter of what an integrator's fields mean.
- Does not imply that using Avalon makes a character portable — portability of
  any kind stays opt-in and per-entity, per
  [`./future-layers.md`](./future-layers.md).
- Does validate exposed data against a published schema, and does parse
  `.proto` IDL to do so. Compiling to a language binding (codegen) is out of
  scope; parsing is only ever for structural validation.
- Does not extend `IntegratorBinding` or the achievement `schema` field to carry
  general integrator data; both stay exactly as narrowly scoped as they are today.

## Current implementation

- `avalon_protocol::integrator_schemas::IntegratorSchemaVersion`
  (`crates/protocol/src/integrator_schemas.rs`) is the one domain type: integrator id,
  raw `.proto` source, monotonic `version: u32`, `published_at`, and an
  optional `superseded_by: Option<GlobalId>` lineage pointer — a
  generalization of the `AchievementDefinition.schema`/`version` precedent
  from a bare reference field to the schema description itself.
  No `IntegratorSpace`/`Mapping`/`Entity` type exists yet outside what's
  described below.
- **`proto_source` is actually parsed.** `crate::proto_schema`
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
  direction) live on `integrator_schemas`/`indexer_integrator_schemas`.
  Both default to fully open (`"public"`, `{}`) — a schema published before
  visibility support landed keeps behaving exactly as it did before.
  `field_visibility`'s keys are validated against the parsed root message's
  real field names at publish time (`proto_schema::validate_field_visibility_keys`);
  a nonexistent field name is rejected, not silently accepted.
- **Instance-data publication**: `POST
  /integrations/{slug}/schemas/{version}/data` (`crates/server/src/integrator_data.rs`) —
  a `game_data.published` event kind, with `integrator_data_instances` +
  `indexer_integrator_data_instances` tables mirroring the
  `integrator_schemas`/`indexer_integrator_schemas` pairing exactly (append-only,
  `superseded_by` lineage, no PATCH).
- **Instance deletion**: `DELETE
  /integrations/{slug}/schemas/{version}/data/{subject}` — a deleted
  character (or any other published instance) gets a `game_data.deleted`
  tombstone, never a physical delete; see
  [`./revocation.md`](./revocation.md)'s "Entity/instance deletion"
  section for the full mechanics.

  Write auth needs three things together: `authenticate_owning_integrator` (the
  caller must *be* `{slug}`, same guard `publish_schema_version` uses); an
  active binding from the subject identity to that integrator
  (`authz::has_active_binding`, mirroring `achievements::issue_attestation`'s
  "the user's own consent" pattern); and the resolved schema's own
  `integrator_id` matching the caller. The submitted `instance` JSON is then
  validated against the schema's parsed root message via
  `protobuf-json-mapping` (`proto_schema::validate_instance_json`) — unknown
  fields, wrong types, and (for a proto2-style schema) missing `required`
  fields are all rejected with `AppError::InstanceSchemaMismatch`, never
  stored.
- **Read**: `GET /identities/{id}/integrator-data`
  (`integrator_data::get_identity_integrator_data`) — public, unauthenticated, same
  posture `GET /attestations/{id}` already has. Reads the indexer's own
  projection (`avalon_indexer::projections::integrator_data_instances`), never raw
  ledger/outbox data, and applies the bidirectional visibility rule
  (`integrator_data::resolve_visible_fields`, a pure function unit-tested
  directly) per instance: a field is included iff `default_visibility` is
  `"public"` and the field isn't marked `"private"`, or `default_visibility`
  is `"private"` and the field is marked `"public"`. Only literal top-level
  JSON key matching — no nested-field visibility in this pass (documented
  limitation, not silently attempted). The Hub reads this endpoint too:
  `UserProfile.vue`'s "Published by connected apps" card, rendering
  each visible instance generically (field name -> value, no per-schema
  custom rendering yet) — a profile with nothing published, and one with
  published data none of it currently visible to the caller, render
  identically (an empty state), matching this endpoint's own
  by-design "can't tell those two apart" posture.
- **Rust SDK codegen**: `#[derive(AvalonSchema)]`
  (in the Rust SDK's `schema-derive` crate, `avalon-schema-derive`) generates
  a struct's `.proto` message text plus its
  `default_visibility`/`field_visibility` maps, so an integrator using the
  Rust SDK never hand-writes `.proto` source or the raw publish request — see
  [sdk.md](../../sdks/rust/README.md) for the macro's supported-type scope
  and `Session::publish_schema_version`/`publish_instance`.
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
  `crates/server/src/integrator_schemas.rs::schema_ref`.
- `crates/server/src/integrator_schemas.rs` — `POST /integrations/{slug}/schemas`
  (publish the next version, integrator-credential-authenticated the same way
  `achievements.rs` authenticates achievement-definition writes — proving
  the integrator owns the slug, not an identity-granted capability), `GET
  /integrations/{slug}/schemas` (list, public), `GET
  /integrations/{slug}/schemas/{version}` (one version, public). `integrator_schemas`
  is the request-serving projection; `proto_source` is never updated once
  inserted.
- The [Integrator Registry](./registry.md)'s read model covers schema
  discovery: `crates/indexer/src/projections/integrator_schemas.rs` projects
  `game_schema.published` into `indexer_integrator_schemas`, queried by
  `list_for_integrator`.
- **Schema-to-schema mapping model** — `crates/server/src/integrator_schema_mappings.rs`:
  `POST /integrations/{slug}/mappings` (publish, integrator-credential-
  authenticated, same ownership check as schema publication), `GET
  /integrations/{slug}/mappings` (list, public), `GET
  /integrations/{slug}/mappings/{seq}` (one mapping, public). A mapping
  references two of the publishing integrator's own already-published
  schema versions (`from_schema_id`/`to_schema_id`, validated for
  existence and ownership, never a DB-enforced foreign key — same
  precedent `superseded_by` set) plus a `field_correspondence` old-field
  -> new-field map and free-text `description` for whatever that map can't
  capture. No lineage/superseding concept, unlike schema versions — each
  mapping is a standalone, immutable fact
  (`crates/protocol/src/integrator_schema_mappings.rs`). Discoverable
  through the same indexer-projection machinery schema versions use
  (`crates/indexer/src/projections/integrator_schema_mappings.rs`,
  `game_schema_mapping.published`). SDK: `Session::publish_mapping`,
  `AvalonClient::list_schema_mappings`/`get_schema_mapping`. Verified live
  against a real server, including the ownership-rejection and
  nonexistent-schema-rejection paths
  (`crates/server/tests/integrator_schema_mappings.rs`, `--ignored`).
