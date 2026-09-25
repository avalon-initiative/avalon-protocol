# Integrator Registry and Network Intelligence

**The registry exposes facts with explicit definitions. It never publishes a
score, a ranking, or a trust judgment.** Metrics are derived from protocol
activity wherever possible, labeled by how they were obtained, and aggregated so
that no per-user data is exposed. **Statistics inform a consumer's trust
decision; they do not determine it.**

## Not a static list

The registry is the network's intelligence layer about participating integrators and
issuers, not a directory of names:

```text
Avalon Integrator Registry
  ├── Integrator identity              (game:ashen-realms)
  ├── Issuer keys and key history
  ├── Status                     (active / suspended / revoked / deprecated)
  ├── Capabilities               (what Avalon features the integrator supports/requests)
  ├── Public metadata            (name, developer, website — self-reported)
  ├── Published schema versions  (Integrator Space schema publication)
  ├── Durable activity metrics   (derived from events)
  ├── Recognition relationships  (who recognizes whom, for what)
  └── Aggregate network analytics
```

Identity, keys, and status come from [integrators and issuers](./issuers.md).
Everything else is a projection built by the [indexer](./query-and-indexing.md).

## Beyond games: integrator category

Games are the first and most-developed integrator category, not the only one
long-term: websites and other non-game applications can register too.
`category` (`IntegratorCategory`: `game` / `app` / `service`) is an additive
field on registration; a registrant that omits it is a `game`, so every
existing registration and caller is unaffected. The Hub's directory (below)
reflects this with category tabs; only `Games` has real registrants today.

The registrant is an `Integrator` (`IntegratorId`, the `integrators` table,
`/integrations`), and "game" now appears only where it genuinely means
gaming. Three things deliberately kept their original spelling, and are not
oversights:

- **Ledger event kinds** (`game.registered`, `game.binding_established`,
  `game.binding_ended`, `game_schema.published`, `game_data.published`) and
  their payload keys. These are hash-chained into committed
  `ledger_entries` and replayed verbatim by the indexer — see
  [protocol events](./protocol-events.md).
- **The `game` `GlobalId` namespace**, so ids like
  `game:ashen-realms:achievement:dragon_slayer` keep resolving.
- **`Issuer::Game` / `IntegratorCategory::Game`**, whose variant names *are*
  the category vocabulary, sitting alongside `App` and `Service`.

## Derive, don't trust

```text
Integrator registers
       ↓
Players establish bindings
       ↓
Protocol events (bindings, attestations, guild associations, recognition)
       ↓
Indexer
       ↓
Aggregate statistics
```

"Players" is not a number an integrator reports. It is *distinct Avalon identities with
an active [binding](./bindings.md) to the integrator*, observed through protocol
activity. Every published metric carries its definition and a class label.

## Metric definitions

| Metric | Definition | Class |
|---|---|---|
| players | distinct identities with an active `IntegratorBinding` | durable-derived |
| total players ever | distinct identities that ever had a binding | durable-derived |
| achievements issued / revoked | count of `achievement.issued` / `.revoked` by this issuer | durable-derived |
| unique achievement holders | distinct subjects with ≥1 valid attestation from this issuer | durable-derived |
| achievement popularity | holders per achievement id | durable-derived |
| cross-integrator players | bound identities that also hold a binding elsewhere | durable-derived |
| recognizing integrators | integrators with a public recognition relationship to this issuer | durable-derived |
| guilds with players here | distinct guilds with ≥1 member bound to this integrator | durable-derived |
| guild members associated | distinct guild members bound to this integrator | durable-derived |
| integrator event participation | attestations with the game-event schema | durable-derived |
| key lifecycle / status / registration history | from issuer events | durable-derived |
| players online now | from presence | **realtime** |
| anything supplied by the integrator | e.g. genre, website | **self-reported** |

Realtime numbers are never stored as durable metrics. Self-reported
fields are shown as self-reported. Guild metrics use the association phrasing
from [guilds](./guilds.md): "N Avalon guilds have members who play Integrator A",
never "Integrator A has N guilds".

## Schema discovery

Not a metric — an integrator's published [Integrator Space](./integrator-space.md) schema
versions are self-authored facts (integrator id, version, `.proto` source,
published-at, `superseded_by` lineage), surfaced the same way everything
else in this document is: an indexer projection derived from durable
events, never a second source of truth. `game_schema.published` is decoded
and applied by `crates/indexer/src/projections/integrator_schemas.rs`, the same
decode/apply shape every other projection in that crate uses; `list_for_integrator`
returns an integrator's published versions oldest-first, empty for an integrator that has
never published. This reuses the existing registry-projection pattern rather
than a dedicated schema-discovery service.

## Statistics inform trust; they do not determine it

A consuming integrator may eventually write a policy like "accept tournament results
from issuers with at least X bound players and Y recognizing integrations". The
registry provides the inputs. It does not enforce "integrators above X are trusted".

An integrator with 10 players is not automatically malicious. An integrator with 10,000,000 is
not automatically trustworthy. There is no

```text
Avalon Integrator Score: 92/100
```

and there will not be one without its own explicit design and decision, kept
distinguishable from objective network facts. See the
[trust model](./trust-model.md).

## Recognition relationships and the network graph

When an integrator chooses to publish its recognition policy, the registry records it
as a fact:

```text
Integrator A recognizes Integrator B    achievements, tournament results
Integrator A recognizes Integrator C    tournament results
Integrator B recognizes Integrator A    achievements
```

Over time this forms a graph — shared players, guilds spanning integrators,
integrator events connecting integrators, issuers recognized by others. The graph is
valuable network intelligence. It is not authority: "72 integrators recognize Integrator A"
is an input to someone's decision, never a conclusion Avalon draws. Recognition
is also not validity — a claim can be authentic and valid while recognized by
nobody.

## Sybil and metric manipulation

An integrator can create a million identities, bind them, issue itself achievements,
and appear highly active. Any metric that might influence trust is attackable.
Levers that raise the cost, none of which are implemented or weighted today:

- identity quality and account age
- cross-integrator participation (bindings to unrelated integrators)
- attestations from other issuers about the same identities
- issuer history and time in the network
- the cost of manipulation relative to the benefit

This is recorded as an architectural concern, not solved. No metric is scored or
weighted to compensate; doing so would be a reputation system by another name.

## Privacy

The registry publishes aggregates ("2,481,392 unique players"), never
per-identity lists. Which identities are bound to an integrator is visible only under
the [visibility](./privacy.md) rules that apply to those identities.

**A minimum cohort size.** An aggregate that's small enough stops being
anonymous — `unique_achievement_holders: 1` on an obscure achievement
identifies one specific real person as surely as a name would, even though no
name ever appears in the response. Every metric below a configurable floor
(`AVALON_REGISTRY_MIN_COHORT`, default 5) is coarsened to the floor itself,
marked `exact: false`, rather than returned as the real sub-floor count —
see [privacy.md](./privacy.md) and [Current implementation](#current-implementation)
below for where this is enforced.

## External read surface

The registry is meant to be read by more than the Hub — a game's own
tooling, a researcher, a future client that isn't the Hub — without
scraping rendered pages or getting direct database access. `GET
/registry/{slug}` is that dedicated, standalone contract: the exact same
data `GET /integrations/{slug}/registry` already serves (that route stays,
unchanged, for the Hub and anything else already depending on it), under
its own top-level namespace so it reads as a real external surface rather
than something incidental to integrator management.

**Stability policy.** This repo has no route-versioning scheme anywhere
yet (no `/v1/` prefix, no `Accept`-header negotiation), and inventing one
for a single endpoint would be inconsistent with everything else. Instead:
a response field's *meaning* is permanent once shipped, the same
"permanent once shipped" discipline `Capability` wire strings
(`crates/protocol/src/permissions.rs`) and ledger event-kind strings
already follow. Concretely:

- New metrics are additive — a new field can appear; an existing field's
  `value`/`definition`/`class` never silently changes what it measures.
- A genuinely breaking change (redefining what an existing field counts,
  removing one) gets a new field name or a new route, never an in-place
  change to what callers already depend on.
- `definition`/`class` on every field are part of the contract, not
  incidental — a caller is meant to render them, not hardcode field-name
  assumptions about what `players` means independent of what the response
  itself says.

**What's guaranteed stable today:** the five fields `GET /registry/{slug}`
currently returns (`players`, `total_players_ever`, `achievements_issued`,
`achievements_revoked`, `unique_achievement_holders`), each as `{ value,
definition, class, exact }`, and that no response ever carries per-identity
data. **What can still change:** the route family may grow siblings (a
listing route, recognition relationships) without those being covered by
today's stability promise until they exist.

The Rust SDK's `AvalonClient::registry(slug)` is the thin typed client for
this — no `Session`/`authenticate()` needed, since the route itself is
public and unauthenticated. The C# SDK does not mirror this yet.

## Current implementation

- `crates/protocol/src/integrators.rs` — `Integrator { id, slug, name, developer,
  registered_at, category }`, `IntegratorRegistration`, `IntegratorCredential`.
  `category` (`IntegratorCategory`) defaults to `Game`. No
  capabilities-supported or metrics.
- `crates/indexer/src/lib.rs` — the `Indexer` trait, dispatched by
  `crates/indexer/src/postgres.rs::PostgresIndexer`, to one projection
  module per read model under `crates/indexer/src/projections/`. Schema
  discovery (`integrator_schemas.rs`) is the first Integrator-Registry-facing
  projection; profiles/friendships/guild rosters/attestations are the
  others, most of them not registry-shaped.
- **First slice of the metric table implemented**: `players`,
  `total players ever`, `achievements issued`, `achievements revoked`, and
  `unique achievement holders` — all `durable-derived` — are real.
  `players`/`total players ever` come from
  `crates/indexer/src/projections/integrator_bindings.rs` (its own
  `indexer_integrator_bindings` table, decoded from
  `game.binding_established`/`game.binding_ended`). The three achievement
  metrics come from `crates/indexer/src/projections/attestations.rs`,
  reading the existing `indexer_attestations` table.

  `crates/indexer/src/registry.rs::compute_for_integrator` composes both into a
  `IntegratorRegistryMetrics` where every field is a `{ value, definition, class }`
  triple, never a bare number, and returns zeros (not an error) for an integrator
  with no activity. It's exposed publicly and unauthenticated at
  `GET /integrations/{slug}/registry` (`crates/server/src/registry.rs`) — a separate
  endpoint from `GET /integrations/{slug}` on purpose, so the class-label
  contract can't be accidentally skipped by flattening metrics alongside
  plain registration fields. Fixture-based unit tests per metric (no live
  Postgres) live alongside each projection module.
- **Minimum cohort size**: `crates/indexer/src/registry.rs`'s `coarsen` is the
  single enforcement point every metric in `compute_for_integrator` passes
  through before `Metric`/`MetricResponse` ever leave the process — a raw
  count at or above `min_cohort()` (`AVALON_REGISTRY_MIN_COHORT` env var,
  default `DEFAULT_MIN_COHORT` = 5) ships exactly with `exact: true`; a
  nonzero count below it is replaced with the floor itself and `exact:
  false`. `0` is never coarsened. There is exactly one caller today (`GET
  /integrations/{slug}/registry`, no filter params), so no live
  filter-chain-narrows-a-cohort path exists yet to exploit — `coarsen`
  checks the final computed count regardless of how it was produced, so a
  future filtered query composes into the same enforcement point rather
  than needing its own.
- **`GET /integrations` — the Hub integrator directory's list endpoint**:
  public, unauthenticated, cursor-paginated the same way `GET
  /guilds/discover` already is — `crates/server/src/integrators.rs`'s
  `build_integrators_list_query` mirrors `guilds.rs`'s
  `build_discover_query` keyset-pagination shape exactly. `q=`/`sort=`
  (`newest` default | `name`, no ranking/score option)/`limit=`/`cursor=`,
  returning each integrator's public summary
  (id/slug/name/developer/registered_at/status) — the same fields `GET
  /integrations/{slug}` exposes, just listed, with `requested_capabilities`
  left off since a directory card has no reason to fetch a field it
  doesn't show. A pragmatic query over the `integrators` table directly,
  not yet a real indexer read model, the same pragmatic call
  `discover_guilds` already made for guilds.
- **Hub directory: "Connected Apps"**: the Hub nav entry and route are
  `/integrations` (`avalon-hub/apps/hub/src/router/index.ts`); `/games` and
  `/games/:slug` still resolve, as redirects, so existing deep links don't
  404. `IntegrationDirectory.vue` has category tabs (Games / Apps /
  Services) filtering the fetched list client-side by `category`; only
  `Games` has real registrants today, so the other tabs render correctly
  empty rather than being hidden.
- **Server public API: `/integrations` canonical.**
  `crates/server/src/integrators.rs`/`lib.rs` route
  `GET /integrations`/`GET /integrations/{slug}` as the canonical reads;
  `GET /games`/`GET /games/{slug}` remain working as real HTTP redirects to
  the new paths (preserving the query string). `/integrations` is the only
  path for writes and the `x-avalon-integrator-*` header spelling is the
  only one accepted. The Hub's API client
  (`avalon-hub/apps/hub/src/api/client.ts`) calls `/integrations`/`/integrations/{slug}`
  directly.
- **Hub integrator directory + per-integrator profile page**:
  `avalon-hub/apps/hub/src/views/IntegrationDirectory.vue` lists `GET /integrations`
  results with a search box and name/newest sort toggle — no "recommended"
  ordering. `IntegrationProfile.vue` (`/integrations/:slug`, with
  `/games/:slug` redirecting) renders `GET /integrations/{slug}`'s public
  fields plus `GET /integrations/{slug}/registry`'s five metrics via
  `AvalonMetricTile` — value, definition, and class label together, never a
  bare number. A non-`active` `status` renders as a visibly distinct badge
  rather than reading the same as `active`; no key-history UI is built here
  yet. the UI library's `AvalonIntegratorCard` and `AvalonMetricTile` are
  the reusable components for this, in the existing
  components/styles/stories split.
- **Recognition relationships**: `crates/server/src/recognitions.rs` —
  `POST /integrations/{slug}/recognitions` (`{ recognized_slug, scope: [...] }`,
  challenge-response-authenticated, same `authenticate_owning_integrator`
  pattern `integrator_schemas.rs` established) publishes or updates a
  directional recognition; `POST /integrations/{slug}/recognitions/revoke`
  marks it revoked without deleting the row (the fact "A used to recognize
  B" stays visible). `scope` is a free-form string list, never a fixed
  protocol vocabulary or a score. `GET /integrations/{slug}/recognitions`
  (who `slug` recognizes) and `GET /integrations/{slug}/recognized-by` (who
  recognizes `slug`) are both public, unauthenticated, real queryable graph
  edges — not a field collapsed onto either integrator's own row. Durable
  (`integrator.recognition_published`/`.recognition_revoked` events,
  `crates/indexer/src/projections/integrator_recognitions.rs`'s own
  `indexer_integrator_recognitions` projection), mirroring
  `integrator_schemas`'s server-table-plus-indexer-projection split.
  Live-tested end to end (`crates/server/tests/recognitions.rs`): publish,
  read from both directions, confirm it isn't symmetric, revoke, confirm it
  disappears from both directional reads; plus the two rejection cases
  (recognizing as a different integrator than the authenticated caller;
  recognizing yourself).

A parallel, optional naming path exists alongside `game:<slug>` for
self-certifying (`node:<key-hash>`) shard ids — domain-proven
`NameBindingClaim`s, resolved by `POST/GET /shards/.../name-claims`, never
consulted by or touching this registry's own resolution. See
[`network-trust-anchors.md`](./network-trust-anchors.md)'s "Domain-proven
names" section.

## Open questions

Still open: the rest of the metric table above (achievement popularity,
cross-integrator players, guild-association metrics, integrator event
participation, key lifecycle/status/registration history) and a realtime
"players online now" metric. Schema discovery is likewise still queryable
only via `avalon_indexer::projections::integrator_schemas::list_for_integrator`,
not yet folded into the registry endpoint. Neither issuer key history nor
recognition relationships are rendered anywhere in the Hub yet.
</content>
</invoke>
