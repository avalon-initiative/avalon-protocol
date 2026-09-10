# Game Registry and Network Intelligence

**The registry exposes facts with explicit definitions. It never publishes a
score, a ranking, or a trust judgment.** Metrics are derived from protocol
activity wherever possible, labeled by how they were obtained, and aggregated so
that no per-player data is exposed. **Statistics inform a consumer's trust
decision; they do not determine it.**

## Not a static list

The registry is the network's intelligence layer about participating games and
issuers, not a directory of names:

```text
Avalon Game Registry
  ├── Game identity              (game:ashen-realms)
  ├── Issuer keys and key history
  ├── Status                     (active / suspended / revoked / deprecated)
  ├── Capabilities               (what Avalon features the game supports/requests)
  ├── Public metadata            (name, developer, website — self-reported)
  ├── Published schema versions  (Game Space schema publication, #255)
  ├── Durable activity metrics   (derived from events)
  ├── Recognition relationships  (who recognizes whom, for what)
  └── Aggregate network analytics
```

Identity, keys, and status come from [games and issuers](./games-and-issuers.md).
Everything else is a projection built by the [indexer](./query-and-indexing.md).

## Derive, don't trust

```text
Game registers
       ↓
Players establish bindings
       ↓
Protocol events (bindings, attestations, guild associations, recognition)
       ↓
Indexer
       ↓
Aggregate statistics
```

"Players" is not a number a game reports. It is *distinct Avalon identities with
an active [binding](./game-bindings.md) to the game*, observed through protocol
activity. Every published metric carries its definition and a class label.

## Metric definitions

| Metric | Definition | Class |
|---|---|---|
| players | distinct identities with an active `GameBinding` | durable-derived |
| total players ever | distinct identities that ever had a binding | durable-derived |
| achievements issued / revoked | count of `achievement.issued` / `.revoked` by this issuer | durable-derived |
| unique achievement holders | distinct subjects with ≥1 valid attestation from this issuer | durable-derived |
| achievement popularity | holders per achievement id | durable-derived |
| cross-game players | bound identities that also hold a binding elsewhere | durable-derived |
| recognizing games | games with a public recognition relationship to this issuer | durable-derived |
| guilds with players here | distinct guilds with ≥1 member bound to this game | durable-derived |
| guild members associated | distinct guild members bound to this game | durable-derived |
| game event participation | attestations with the game-event schema | durable-derived |
| key lifecycle / status / registration history | from issuer events | durable-derived |
| players online now | from presence | **realtime** |
| anything supplied by the game | e.g. genre, website | **self-reported** |

Realtime numbers are never stored as durable metrics
([#78](https://github.com/LunarVagabond/avalon-protocol/issues/78)). Self-reported
fields are shown as self-reported. Guild metrics use the association phrasing
from [guilds](./guilds.md): "N Avalon guilds have members who play Game A",
never "Game A has N guilds".

## Schema discovery (#255)

Not a metric — a game's published [Game Space](./game-space.md) schema
versions are self-authored facts (game id, version, `.proto` source,
published-at, `superseded_by` lineage), surfaced the same way everything
else in this document is: an indexer projection derived from durable
events, never a second source of truth. `game_schema.published` is decoded
and applied by `crates/indexer/src/projections/game_schemas.rs`, the same
decode/apply shape every other projection in that crate uses; `list_for_game`
returns a game's published versions oldest-first, empty for a game that has
never published. This is the "reuse the existing registry-projection
pattern" discovery surface #181 left open, rather than a dedicated
schema-discovery service.

## Statistics inform trust; they do not determine it

A consuming game may eventually write a policy like "accept tournament results
from issuers with at least X bound players and Y recognizing integrations". The
registry provides the inputs. It does not enforce "games above X are trusted".

A game with 10 players is not automatically malicious. A game with 10,000,000 is
not automatically trustworthy. There is no

```text
Avalon Game Score: 92/100
```

and there will not be one without its own explicit design and decision, kept
distinguishable from objective network facts. See the
[trust model](./trust-model.md).

## Recognition relationships and the network graph

When a game chooses to publish its recognition policy, the registry records it
as a fact:

```text
Game A recognizes Game B    achievements, tournament results
Game A recognizes Game C    tournament results
Game B recognizes Game A    achievements
```

Over time this forms a graph — shared players, guilds spanning games,
game events connecting games, issuers recognized by others. The graph is
valuable network intelligence. It is not authority: "72 games recognize Game A"
is an input to someone's decision, never a conclusion Avalon draws. Recognition
is also not validity — a claim can be authentic and valid while recognized by
nobody.

## Sybil and metric manipulation

A game can create a million identities, bind them, issue itself achievements,
and appear highly active. Any metric that might influence trust is attackable.
Levers that raise the cost, none of which are implemented or weighted today:

- identity quality and account age
- cross-game participation (bindings to unrelated games)
- attestations from other issuers about the same identities
- issuer history and time in the network
- the cost of manipulation relative to the benefit

This is recorded as an architectural concern, not solved. No metric is scored or
weighted to compensate; doing so would be a reputation system by another name.

## Privacy

The registry publishes aggregates ("2,481,392 unique players"), never
per-identity lists. Which identities are bound to a game is visible only under
the [visibility](./privacy.md) rules that apply to those identities.

## Today in the repo

- `crates/protocol/src/games.rs` — `Game { id, slug, name, developer,
  registered_at }`, `GameRegistration`, `GameCredential`. No status, keys,
  capabilities-supported, or metrics.
- `crates/indexer/src/lib.rs` — the `Indexer` trait, dispatched by
  `crates/indexer/src/postgres.rs::PostgresIndexer`, to one projection
  module per read model under `crates/indexer/src/projections/`. Schema
  discovery (`game_schemas.rs`, #255) is the first Game-Registry-facing
  projection; profiles/friendships/guild rosters/attestations are the
  others, most of them not registry-shaped.
- **First slice of the metric table implemented (#261)**: `players`,
  `total players ever`, `achievements issued`, `achievements revoked`, and
  `unique achievement holders` — all `durable-derived` — are real. `players`
  / `total players ever` come from `crates/indexer/src/projections/game_bindings.rs`
  (its own `indexer_game_bindings` table, decoded from
  `game.binding_established`/`game.binding_ended`, migration
  `0035_indexer_game_bindings`). The three achievement metrics come from
  `crates/indexer/src/projections/attestations.rs`, reading the existing
  `indexer_attestations` table. `crates/indexer/src/registry.rs::compute_for_game`
  composes both into a `GameRegistryMetrics` where every field is a
  `{ value, definition, class }` triple, never a bare number, and returns
  zeros (not an error) for a game with no activity. Exposed publicly and
  unauthenticated at `GET /games/{slug}/registry`
  (`crates/server/src/registry.rs`) — a separate endpoint from `GET
  /games/{slug}` on purpose, so the class-label contract can't be
  accidentally skipped by flattening metrics alongside plain registration
  fields. Fixture-based unit tests per metric (no live Postgres) live
  alongside each projection module.
- **`GET /games` — the Hub game directory's list endpoint (#270)**:
  public, unauthenticated, cursor-paginated the same way `GET
  /guilds/discover` already is (#154) — `crates/server/src/games.rs`'s
  `build_games_list_query` mirrors `guilds.rs`'s `build_discover_query`
  keyset-pagination shape exactly. `q=`/`sort=` (`newest` default | `name`,
  no ranking/score option) /`limit=`/`cursor=`, returning each game's
  public summary (id/slug/name/developer/registered_at/status) — the same
  fields `GET /games/{slug}` exposes, just listed, with
  `requested_capabilities` left off since a directory card has no reason
  to fetch a field it doesn't show. Milestone-1 stand-in over the `games`
  table, not #42's real indexer read model, same pragmatic call
  `discover_guilds` already made for guilds.
- **Hub game directory + per-game profile page (#270, first slice of
  #90)**: `apps/hub/src/views/GameDirectory.vue` lists `GET /games`
  results with a search box and name/newest sort toggle — no
  "recommended" ordering, matching #89's invariant. `GameProfile.vue`
  (`/games/:slug`) renders `GET /games/{slug}`'s public fields plus `GET
  /games/{slug}/registry`'s five metrics via `AvalonMetricTile` — value,
  definition, and class label together, never a bare number. A
  non-`active` `status` renders as a visibly distinct badge
  (`AvalonGameCard`/`GameProfile.vue`'s status badge) rather than reading
  the same as `active`; no key-history UI is built here (blocked on the
  still-open #80). `packages/ui`'s `AvalonGameCard` and `AvalonMetricTile`
  are the new reusable components, in the existing
  components/styles/stories split.
- Still open: the rest of the table above (achievement popularity,
  cross-game players, recognizing games, guild-association metrics, game
  event participation, key lifecycle/status/registration history),
  recognition relationships (needs a new protocol event, #94), the
  privacy/cohort-size floor ([#96](https://github.com/LunarVagabond/avalon-protocol/issues/96)),
  and the public/external read surface
  ([#95](https://github.com/LunarVagabond/avalon-protocol/issues/95)) that
  #261's endpoint is not itself. Schema discovery
  (#255) is likewise still queryable only via
  `avalon_indexer::projections::game_schemas::list_for_game`, not yet
  folded into the registry endpoint. Issuer key history and recognition
  relationships are still not rendered anywhere in the Hub — #270 is only
  the first buildable slice of #90, not the full ticket.

## Decisions and tickets

- [#89](https://github.com/LunarVagabond/avalon-protocol/issues/89) — registry read
  model: derived metrics with explicit definitions.
- [#90](https://github.com/LunarVagabond/avalon-protocol/issues/90) — Hub game
  discovery + per-game profile pages.
- [#76](https://github.com/LunarVagabond/avalon-protocol/issues/76) — ADR: trust
  model (statistics inform, never determine).
- [#83](https://github.com/LunarVagabond/avalon-protocol/issues/83) — game
  bindings, the unit "players" counts.
- [#84](https://github.com/LunarVagabond/avalon-protocol/issues/84) — issuer
  identity, keys, and status shown in the registry.
- [#41](https://github.com/LunarVagabond/avalon-protocol/issues/41) — Epic:
  Query/Index Layer.
- [#255](https://github.com/LunarVagabond/avalon-protocol/issues/255) — Game
  Schema Publication; the schema-discovery projection described above.
- [#181](https://github.com/LunarVagabond/avalon-protocol/issues/181) —
  Decision: game-defined schema model, representation, and versioning
  strategy; left discovery surface to implementation, resolved by #255 as
  the registry read model, not a dedicated endpoint.
- [#261](https://github.com/LunarVagabond/avalon-protocol/issues/261) —
  first slice: the five binding/achievement `durable-derived` metrics and
  `GET /games/{slug}/registry`, described above.
- [#270](https://github.com/LunarVagabond/avalon-protocol/issues/270) —
  first buildable Hub slice on #90: `GET /games`, the game directory, and
  the per-game profile page reading #261's metrics, described above.
- [#94](https://github.com/LunarVagabond/avalon-protocol/issues/94) — Epic
  this ticket and the rest of the registry work sit under.
- [#96](https://github.com/LunarVagabond/avalon-protocol/issues/96) —
  privacy/cohort-size floor, not yet applied to #261's endpoint.
- [#95](https://github.com/LunarVagabond/avalon-protocol/issues/95) — the
  public/external read surface #261's endpoint feeds into, not yet built.
