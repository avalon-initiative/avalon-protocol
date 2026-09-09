# Game Space: Schemas, Publication, and Mapping

**A binding says a player participates in a game. A Game Space says what the
game has chosen to describe to Avalon about its own data model, and how much
of it is actually exposed.** Neither one gives Avalon an opinion about what a
"level" or a "class" is. This document is the design for the layer that lets a
game make its data *structurally describable, versioned, discoverable, and
attributable to itself* without Avalon standardizing what that data means.

**Nothing in this document is built. There is no `Schema` type, no mapping
type, and no Game Space concept anywhere in `protocol` today.** This is the
architecture for a gap identified by audit, not a description of shipped
code — see [Today in the repo](#today-in-the-repo).

## Game Space is not a new participation record

[Game bindings](./game-bindings.md) already establish "this identity
participates in this game," deliberately with no game-data field, per
[#67](https://github.com/LunarVagabond/avalon-protocol/issues/67). A **Game
Space** is a different thing: it belongs to the *game*, not to a
player-game pair, and it is where a game optionally publishes *how its data is
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
**The game developer owns the semantic transformation.** Avalon's job is
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
**No representation, validation mechanism, or serialization format is decided
by this document** — see [Decisions and tickets](#decisions-and-tickets).

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

If schema publication is ever built, publication *metadata* (schema id,
version, owner, deprecation status) is the kind of durable-derived fact the
[Game Registry](./game-registry.md) already exists to surface — the same
place capabilities, keys, and activity metrics live today. Full schema
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
- Does not pick a schema representation, validation library, or serialization
  format. That remains open — see below.
- Does not extend `GameBinding` or the achievement `schema` field to carry
  general game data; both stay exactly as narrowly scoped as they are today.

## Today in the repo

- No `Schema`, `GameSpace`, `Mapping`, or `Entity` type exists anywhere in
  `crates/protocol/src/`. The only schema-shaped field in the whole crate is
  `AchievementDefinition.schema: Option<GlobalId>` +
  `version: u32` ([`./achievements-and-attestations.md`](./achievements-and-attestations.md)),
  scoped to achievement/attestation shape, not general game data.
- `GlobalId::new(namespace, owner, kind, key)`
  (`crates/protocol/src/ids.rs`) is the namespacing mechanism a schema id
  would reuse, unchanged.
- The [Game Registry](./game-registry.md) design and its open tickets
  (epic [#94](https://github.com/LunarVagabond/avalon-protocol/issues/94))
  do not yet cover schema discovery — they cover network facts/metrics only.
- [#82](https://github.com/LunarVagabond/avalon-protocol/issues/82) is a
  versioning policy for `ProtocolEvent.kind`/payload, i.e. Avalon's own
  events — not a mechanism for versioning a game's data model. This document
  proposes the same discipline apply to schemas, not that #82 be widened to
  cover them.
- No open ticket implements any part of this document.
  [#181](https://github.com/LunarVagabond/avalon-protocol/issues/181) is the
  decision that has to close before any implementation ticket under
  [#182](https://github.com/LunarVagabond/avalon-protocol/issues/182) starts.

## Decisions and tickets

- [Proposal §32](../stakeholders/Proposal.md#32-open-questions) — "should
  assets have standardized schemas?" is the existing, still-open question this
  document generalizes from assets to game data broadly.
- [#181](https://github.com/LunarVagabond/avalon-protocol/issues/181) —
  Decision: game-defined schema model, representation, and versioning
  strategy. Tracks the question above; nothing in this document is built
  until this closes.
- [#182](https://github.com/LunarVagabond/avalon-protocol/issues/182) — Epic:
  Game Space & Schema Publication, gated on #181.
- [#67](https://github.com/LunarVagabond/avalon-protocol/issues/67) — ADR:
  identity is separate from game characters; the reason Game Space does not
  become a second game database.
- [#83](https://github.com/LunarVagabond/avalon-protocol/issues/83) — game
  bindings; Game Space is explicitly not an extension of this.
- [#94](https://github.com/LunarVagabond/avalon-protocol/issues/94) — Epic:
  Game Registry & Network Intelligence; schema discovery would live in its
  read surface, not a new service.
- [#82](https://github.com/LunarVagabond/avalon-protocol/issues/82) — protocol
  event kind/versioning policy; the pattern a schema versioning policy would
  follow, not extend.
- [#98](https://github.com/LunarVagabond/avalon-protocol/issues/98) — typed,
  enum-backed capability strings; the pattern a schema/entity-kind catalogue
  would reuse if one is ever built.
