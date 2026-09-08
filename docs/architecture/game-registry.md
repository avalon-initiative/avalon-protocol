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
| tournament participation | attestations with the tournament schema | durable-derived |
| key lifecycle / status / registration history | from issuer events | durable-derived |
| players online now | from presence | **realtime** |
| anything supplied by the game | e.g. genre, website | **self-reported** |

Realtime numbers are never stored as durable metrics
([#78](https://github.com/LunarVagabond/avalon-protocol/issues/78)). Self-reported
fields are shown as self-reported. Guild metrics use the association phrasing
from [guilds](./guilds.md): "N Avalon guilds have members who play Game A",
never "Game A has N guilds".

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
tournaments connecting games, issuers recognized by others. The graph is
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
- `crates/indexer/src/lib.rs` — the `Indexer` trait only; no projections.
- No registry endpoint, no recognition-relationship event, no Hub view.

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
