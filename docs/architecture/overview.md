# Architecture Overview

Avalon is an open interoperability protocol for independently operated integrators.
**Avalon is the railroad between integrators, not a destination.** It owns the
connective layer — identity, social graph, guilds, durable history, provenance —
and nothing else. **Integrators remain sovereign over their own worlds**, and Avalon's
job is to let those worlds recognize the same users and communities without
surrendering control of anything inside them.

This document is the map. Each topic has its own normative doc, listed in
[`README.md`](./README.md). [`../stakeholders/Proposal.md`](../stakeholders/Proposal.md) is the narrative
version and is not repeated here.

## What Avalon is

A shared, opt-in network that independent integrators can connect to for:

- persistent identity and integrator-scoped profiles
- friends, presence, and guilds (with guild chat)
- achievements as verifiable attestations, with provenance and revocation
- integrator and issuer registration, key lifecycle, and an integrator registry
- integrator events and cross-integrator results (tournaments, seasonal championships, ...)
- eventually, portable assets and ownership history

An integrator may be commercial, open source, proprietary, self-hosted, community run,
an MMO, a strategy game, or something that fits no category. Avalon has to be
useful regardless.

## What Avalon is not

| Not this | Because |
|---|---|
| A centralized platform (Roblox) | Avalon does not control identity, distribution, rules, economy, or governance for anyone's integrator. |
| One universal MMO | There may be thousands of independent worlds; none is canonical. |
| A universal character format | Race, class, level, stats, appearance, inventory belong to each integrator. See [`./bindings.md`](./bindings.md). |
| A blockchain game | Settlement is infrastructure. Real-time gameplay never touches it. See [`./settlement.md`](./settlement.md). |
| A universal economy | No universal currency, market, or financial layer is foundational. See [`./future-layers.md`](./future-layers.md). |
| A universal trust oracle | A signature proves who signed a claim, never that the claim is meaningful. See [`./trust-model.md`](./trust-model.md). |
| A "good integrators" ranking | The registry publishes facts with definitions, never a score. See [`./registry.md`](./registry.md). |

## The fundamental question

> **What should survive the death of a particular server or integrator?**

| Integrator state (belongs to the integrator) | Protocol history (belongs to Avalon) |
|---|---|
| HP, XP ticks, movement, physics, combat | achievement issued / revoked |
| NPC state, quests, world state, position | integrator event victory / participation |
| ordinary chat, matchmaking | integrator and issuer registration, key rotation, suspension |
| inventory changes with no interoperability meaning | guild creation, durable membership and role changes |
| game-specific progression and economy | ownership changes, asset provenance, attestations |
| | cross-integrator event results and other explicitly durable facts |

Everything in the left column is high-volume, temporary, and integrator-owned. It is
never a protocol event. Everything in the right column is a durable fact that may
matter outside the integrator that produced it, and it is what the settlement layer
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
- **Realtime / presence** — online state, current integrator, heartbeat. Ephemeral,
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
                         Integrator Registry
                              |
             +----------------+----------------+
             |                |                |
           Integrator A           Integrator B           Integrator C
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

Avalon is the connective tissue. The integrators are the experiences.

## The workspace

Six crates, confirmed by
[#69](https://github.com/LunarVagabond/avalon-protocol/issues/69). Each has a
concrete boundary; none exists merely because a concept has a name.

| Crate | May know about | Must not know about |
|---|---|---|
| `protocol` | domain types, ids, events, traits | Postgres, any chain, HTTP, the server, any node implementation |
| `chain` | settlement: commitments, verification, the ledger/log | general domain semantics (those live in `protocol`) |
| `indexer` | consuming events, projections, read models, aggregates | redefining what an event means |
| `server` | everything — it composes protocol, chain, indexer, realtime, API | being reached around by clients (Hub, integrators use the API/SDK) |
| `sdk` | protocol capabilities, auth, retries, discovery, routing | exposing Postgres, chain RPC, Merkle trees, or node topology to an integrator |
| `cli` | dev/ops workflows: registration, inspection, diagnostics, migrations | being a second server |

Internal growth is by module (`protocol/src/{identity,guilds,achievements,...}.rs`),
not by crate. A new domain such as assets becomes a module of `protocol` unless a
real compilation, ownership, or deployment boundary appears.

## Architectural tests

Every proposed dependency or abstraction is checked against the questions in
[`README.md`](./README.md#architecture-tests) — could `protocol` survive
PostgreSQL, the settlement backend, or HTTP changing; can a node fabricate an
issuer claim (no); can Avalon prove an achievement is meaningful (no); can a
receiving integrator decline a valid claim (yes). The answers there are the
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
   attestations, integrator and issuer registration, a basic registry and Hub, a
   developer API, one tiny demonstration integration.
   [`../stakeholders/Proposal.md#23-the-first-product`](../stakeholders/Proposal.md#23-the-first-product)
2. **SDK** — Rust first, then the languages actual integrations demand.
   [`../stakeholders/Proposal.md#24-phase-2--developer-sdk`](../stakeholders/Proposal.md#24-phase-2--developer-sdk)
3. **External integrators** — independent integrators integrating validates the protocol.
   [`../stakeholders/Proposal.md#25-phase-3--external-games`](../stakeholders/Proposal.md#25-phase-3--external-games)
4. **Portable assets** — provenance, ownership, transfers, recognition.
   [`../stakeholders/Proposal.md#26-phase-4--portable-assets`](../stakeholders/Proposal.md#26-phase-4--portable-assets)
5. **Economy** — only after the network demonstrates real utility.
   [`../stakeholders/Proposal.md#27-phase-5--economy`](../stakeholders/Proposal.md#27-phase-5--economy)

Implementation priority, in order: protocol semantics, authority boundaries,
durable history, verifiable attestations, identity, guild/social primitives,
developer experience, indexing/query, the Hub, settlement optimization, advanced
assets, economy. Settlement throughput is not optimized before the semantics it
settles are correct.

## Today in the repo

- `crates/protocol/src/` — `identity`, `ids`, `integrators`, `integrator_schemas`,
  `guilds`, `social`, `achievements`, `permissions`, `events` modules; pure
  types, no I/O.
- `crates/chain/` — `SettlementProvider` trait and a hash-chained,
  RFC 6962 Merkle-batched Postgres ledger (`postgres.rs`, `merkle.rs`,
  `sth.rs`). Real, not stubbed, and signed at the tree-head level (not
  per-entry) since #39/#210.
- `crates/indexer/` — `PostgresIndexer` (`postgres.rs`, #42) is a real,
  dispatched, idempotent `Indexer`, with one projection module per read
  model under `projections/` (`profiles`, `friendships`, `guild_rosters`,
  `attestations`, `integrator_bindings`, `integrator_schemas`) plus a `registry` module.
  See [`./query-and-indexing.md`](./query-and-indexing.md).
- `crates/server/` — far beyond identity/login/profile now: friends, blocks,
  guilds/channels/events, conversations, achievements, integrator/issuer
  registration and discovery, the integrator registry, a real WebSocket presence
  service (`presence.rs`), settlement/outbox, retention, and recovery all
  have their own module. Most reads still go directly against `server`'s own
  tables rather than through the indexer — #44 closed that migration for
  `profiles`; #506 tracks the rest (`friends.rs`/`guilds.rs`/`connections.rs`).
- `crates/sdk/` — `authenticate()` wired to a live server; friends/presence,
  guilds (roster/channels/chat), and conversations are real, not stubbed;
  `sync_journal`/`submission` (#110/#111) implement offline durability and
  deferred submission. Achievement issuance still returns `NotImplemented`.
- `crates/cli/` — `avalon create-identity`, `login`, `register-integrator`,
  `inspect-ledger`/`inspect-ledger-full`, `outbox-status`, `prune-ledger`.
- `apps/hub` — a real Vue3 client (identity, friends, guilds, conversations,
  integrator discovery), not just scaffolding. `apps/mobile-hub`, `packages/ui`,
  `bindings/csharp` — still scaffolding/skeleton.

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
- Open decisions: [#80](https://github.com/LunarVagabond/avalon-protocol/issues/80)
  issuer keys. Implementation still open on decided questions:
  [#210](https://github.com/LunarVagabond/avalon-protocol/issues/210) (Merkle
  root / Signed Tree Heads, decided by #40) and
  [#201](https://github.com/LunarVagabond/avalon-protocol/issues/201) (guardian
  recovery, decided by #99). Decided since: [ADR #186](https://github.com/LunarVagabond/avalon-protocol/issues/186) —
  no blockchain or validator consensus, a transparency log on Postgres,
  superseding part of [ADR #93](https://github.com/LunarVagabond/avalon-protocol/issues/93);
  [#73](https://github.com/LunarVagabond/avalon-protocol/issues/73) —
  identity is a self-custodied keypair; [#40](https://github.com/LunarVagabond/avalon-protocol/issues/40) —
  log/Merkle design; [#99](https://github.com/LunarVagabond/avalon-protocol/issues/99) —
  identity recovery; [#81](https://github.com/LunarVagabond/avalon-protocol/issues/81) —
  revocation mechanics; [#39](https://github.com/LunarVagabond/avalon-protocol/issues/39) —
  log operator signing (Signed Tree Heads only, not per-entry).
