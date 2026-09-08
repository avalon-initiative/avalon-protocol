# Architecture Overview

Avalon is an open interoperability protocol for independently operated games.
**Avalon is the railroad between games, not a destination.** It owns the
connective layer — identity, social graph, guilds, durable history, provenance —
and nothing else. **Games remain sovereign over their own worlds**, and Avalon's
job is to let those worlds recognize the same players and communities without
surrendering control of anything inside them.

This document is the map. Each topic has its own normative doc, listed in
[`README.md`](./README.md). [`../Proposal.md`](../Proposal.md) is the narrative
version and is not repeated here.

## What Avalon is

A shared, opt-in network that independent games can connect to for:

- persistent player identity and game-scoped profiles
- friends, presence, and guilds (with guild chat)
- achievements as verifiable attestations, with provenance and revocation
- game and issuer registration, key lifecycle, and a game registry
- tournaments and cross-game event results
- eventually, portable assets and ownership history

A game may be commercial, open source, proprietary, self-hosted, community run,
an MMO, a strategy game, or something that fits no category. Avalon has to be
useful regardless.

## What Avalon is not

| Not this | Because |
|---|---|
| A centralized platform (Roblox) | Avalon does not control identity, distribution, rules, economy, or governance for anyone's game. |
| One universal MMO | There may be thousands of independent worlds; none is canonical. |
| A universal character format | Race, class, level, stats, appearance, inventory belong to each game. See [`./game-bindings.md`](./game-bindings.md). |
| A blockchain game | Settlement is infrastructure. Real-time gameplay never touches it. See [`./settlement.md`](./settlement.md). |
| A universal economy | No universal currency, market, or financial layer is foundational. See [`./future-layers.md`](./future-layers.md). |
| A universal trust oracle | A signature proves who signed a claim, never that the claim is meaningful. See [`./trust-model.md`](./trust-model.md). |
| A "good games" ranking | The registry publishes facts with definitions, never a score. See [`./game-registry.md`](./game-registry.md). |

## The fundamental question

> **What should survive the death of a particular server or game?**

| Game state (belongs to the game) | Protocol history (belongs to Avalon) |
|---|---|
| HP, XP ticks, movement, physics, combat | achievement issued / revoked |
| NPC state, quests, world state, position | tournament victory / participation |
| ordinary chat, matchmaking | game and issuer registration, key rotation, suspension |
| inventory changes with no interoperability meaning | guild creation, durable membership and role changes |
| game-specific progression and economy | ownership changes, asset provenance, attestations |
| | cross-game event results and other explicitly durable facts |

Everything in the left column is high-volume, temporary, and game-owned. It is
never a protocol event. Everything in the right column is a durable fact that may
matter outside the game that produced it, and it is what the settlement layer
exists to preserve. [`./protocol-events.md`](./protocol-events.md) draws the line
precisely.

## Three logical verticals

Avalon separates three categories of state. They are responsibilities first and
deployment units second.

```text
                Avalon Network
                       |
       +---------------+---------------+
       |               |               |
   Settlement       Query          Realtime
       |               |               |
      Log          Postgres       Presence
```

- **Settlement / durable history** — canonical protocol facts, commitments,
  provenance. Not the query database. [`./settlement.md`](./settlement.md).
- **Query / indexing** — fast reads, profiles, rosters, discovery, statistics. A
  rebuildable projection of durable history.
  [`./query-and-indexing.md`](./query-and-indexing.md).
- **Realtime / presence** — online state, current game, heartbeat. Ephemeral,
  never ledgered. [`./presence.md`](./presence.md).

**Do not prematurely microservice.** Milestone 1 runs all three inside one
`avalon-server` process. The boundaries exist so an operator can specialize
later (a settlement-only node, an indexer + gateway node) without a rewrite; they
are not an instruction to split today. [`./nodes.md`](./nodes.md) describes the
roles.

## The big picture

```text
                         AVALON NETWORK
                              |
          +-------------------+-------------------+
          |                   |                   |
      Identity             Social              History
          |                   |                   |
          |            +------+------+            |
          |            |             |            |
       Profiles      Friends       Guilds      Attestations
       Bindings        |             |            |
          |            |          Chat            |
          +------------+-------------+------------+
                              |
                         Game Registry
                              |
             +----------------+----------------+
             |                |                |
           Game A           Game B           Game C
             |                |                |
         Characters       Characters       Characters
         World State      World State      World State
         Combat           Combat           Combat
         Economy          Economy          Economy
             |                |                |
             +----------------+----------------+
                              |
                       Avalon SDK/API
                              |
                 +------------+------------+
                 |            |            |
             Query DB      Realtime     Settlement
                 |            |            |
             PostgreSQL     Presence       Log
```

Avalon is the connective tissue. The games are the experiences.

## The workspace

Six crates, confirmed by
[#69](https://github.com/LunarVagabond/avalon-protocol/issues/69). Each has a
concrete boundary; none exists merely because a concept has a name.

| Crate | May know about | Must not know about |
|---|---|---|
| `protocol` | domain types, ids, events, traits | Postgres, any chain, HTTP, the server, any node implementation |
| `chain` | settlement: commitments, verification, the ledger/log | general domain semantics (those live in `protocol`) |
| `indexer` | consuming events, projections, read models, aggregates | redefining what an event means |
| `server` | everything — it composes protocol, chain, indexer, realtime, API | being reached around by clients (Hub, games use the API/SDK) |
| `sdk` | protocol capabilities, auth, retries, discovery, routing | exposing Postgres, chain RPC, Merkle trees, or node topology to a game |
| `cli` | dev/ops workflows: registration, inspection, diagnostics, migrations | being a second server |

Internal growth is by module (`protocol/src/{identity,guilds,achievements,...}.rs`),
not by crate. A new domain such as assets becomes a module of `protocol` unless a
real compilation, ownership, or deployment boundary appears.

## Architectural tests

Every proposed dependency or abstraction is checked against the questions in
[`README.md`](./README.md#architecture-tests) — could `protocol` survive
PostgreSQL, the settlement backend, or HTTP changing; can a node fabricate an
issuer claim (no); can Avalon prove an achievement is meaningful (no); can a
receiving game decline a valid claim (yes). The answers there are the
requirement, not an aspiration.

## Building the smallest thing that doesn't block the future

The eventual vision is large. The first implementation does not need every SDK,
every settlement backend, every node role, a universal economy, consensus,
reputation, sharding, or every social feature. It needs a foundation that is
**correct enough that the project can become large without becoming structurally
wrong**: correct semantics, correct authority boundaries, durable history that
can actually be rebuilt.

Phases, as laid out in the Proposal:

1. **Network** — identity, profiles, friends, guilds and chat, achievements and
   attestations, game and issuer registration, a basic registry and Hub, a
   developer API, one tiny demonstration integration.
   [`../Proposal.md#23-the-first-product`](../Proposal.md#23-the-first-product)
2. **SDK** — Rust first, then the languages actual integrations demand.
   [`../Proposal.md#24-phase-2--developer-sdk`](../Proposal.md#24-phase-2--developer-sdk)
3. **External games** — independent games integrating validates the protocol.
   [`../Proposal.md#25-phase-3--external-games`](../Proposal.md#25-phase-3--external-games)
4. **Portable assets** — provenance, ownership, transfers, recognition.
   [`../Proposal.md#26-phase-4--portable-assets`](../Proposal.md#26-phase-4--portable-assets)
5. **Economy** — only after the network demonstrates real utility.
   [`../Proposal.md#27-phase-5--economy`](../Proposal.md#27-phase-5--economy)

Implementation priority, in order: protocol semantics, authority boundaries,
durable history, verifiable attestations, identity, guild/social primitives,
developer experience, indexing/query, the Hub, settlement optimization, advanced
assets, economy. Settlement throughput is not optimized before the semantics it
settles are correct.

## Today in the repo

- `crates/protocol/src/` — `identity`, `ids`, `games`, `guilds`, `social`,
  `achievements`, `permissions`, `events` modules; pure types, no I/O.
- `crates/chain/` — `SettlementProvider` trait and a hash-chained Postgres
  ledger (`postgres.rs`). Real, not stubbed, but unsigned and unbatched.
- `crates/indexer/src/lib.rs` — the `Indexer` trait only; no implementation yet.
- `crates/server/` — identity registration, login, authenticated profile
  read/update, migrations. Reads its own tables directly; no indexer, no
  realtime yet.
- `crates/sdk/` — `authenticate()` wired to a live server; everything
  capability-gated still returns `NotImplemented`.
- `crates/cli/` — `avalon inspect-ledger`.
- `apps/hub`, `apps/mobile-hub`, `packages/ui`, `bindings/csharp` — scaffolding.

## Decisions and tickets

- [#67](https://github.com/LunarVagabond/avalon-protocol/issues/67) identity is
  separate from game characters
- [#68](https://github.com/LunarVagabond/avalon-protocol/issues/68) attestations
  before blockchain
- [#69](https://github.com/LunarVagabond/avalon-protocol/issues/69) six-crate
  workspace confirmed
- [#70](https://github.com/LunarVagabond/avalon-protocol/issues/70) settlement is
  a public transparency log
- [#74](https://github.com/LunarVagabond/avalon-protocol/issues/74) guilds are
  network-level primitives
- [#75](https://github.com/LunarVagabond/avalon-protocol/issues/75) durable
  history is canonical; query databases are projections
- [#76](https://github.com/LunarVagabond/avalon-protocol/issues/76) attestation
  trust model
- [#77](https://github.com/LunarVagabond/avalon-protocol/issues/77) the Hub is a
  client of the network
- [#78](https://github.com/LunarVagabond/avalon-protocol/issues/78) realtime
  presence is ephemeral
- Open decisions: [#40](https://github.com/LunarVagabond/avalon-protocol/issues/40)
  log design, [#73](https://github.com/LunarVagabond/avalon-protocol/issues/73)
  keypair identity, [#79](https://github.com/LunarVagabond/avalon-protocol/issues/79)
  long-term settlement backend,
  [#80](https://github.com/LunarVagabond/avalon-protocol/issues/80) issuer keys,
  [#81](https://github.com/LunarVagabond/avalon-protocol/issues/81) revocation
  mechanics
