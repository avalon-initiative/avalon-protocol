# Nodes

An Avalon node is infrastructure that transports, indexes, settles, and serves
protocol data. **Nodes are infrastructure providers, not authorities.** **A node
cannot fabricate an issuer's claim or replace an actor's signature.** **Node
capabilities are roles an operator chooses to run, not mandatory separate
binaries.**

## Capabilities

```text
Settlement   — validates and stores durable history (the log)
Indexer      — consumes history, serves query projections
Realtime     — presence and ephemeral connections
Gateway/API  — the SDK/API surface games and clients talk to
```

An operator may run all four in one process, only Indexer + Gateway, only
Settlement, or any other combination. The verticals in
[`./overview.md`](./overview.md) map onto these roles one to one; the point of
keeping the verticals distinct in code is that this specialization is possible
later without a rewrite.

| Node type | Runs | Typical operator |
|---|---|---|
| Settlement / full | the log, verification, mirror sync | a mirror operator, the reference deployment |
| Indexer | projections, registry, historical queries | a hosting provider serving reads |
| Realtime | presence, heartbeat, ephemeral channels | a regional presence service |
| Gateway / API | SDK endpoints, auth, routing | anyone fronting the others |
| Combined | any subset, including all | milestone 1: one `avalon-server` |

## Node authority

A hosted node is not protocol authority. The concrete guarantees:

- A node **cannot fabricate** "Game A issued this achievement". Attestations are
  signed by Game A's registered issuer key
  ([`./games-and-issuers.md`](./games-and-issuers.md)); a node that stores an
  unsigned or wrongly-signed claim has stored something every verifier rejects.
- A node **cannot replace** a signature, alter a settled entry, or drop one
  without detection — the log is hash-chained and, once
  [#40](https://github.com/LunarVagabond/avalon-protocol/issues/40) lands,
  signed and mirrorable.
- A node **cannot act as a player**. Identity mutations are authorized by the
  player's own key ([`./identity.md`](./identity.md),
  [#73](https://github.com/LunarVagabond/avalon-protocol/issues/73)).
- Operator actions that do exist (suspending an issuer at the network level) are
  explicit, audited protocol events with their own trail — never silent edits.
  [`./security-model.md`](./security-model.md) covers the full authority map.

What a node *can* do is the ordinary work of infrastructure: accept, validate,
order, store, index, serve, and mirror.

## Mirrors, not federation

Multiple operators is a goal. It is achieved by **mirroring one public,
verifiable log** — the Certificate Transparency pattern
([#70](https://github.com/LunarVagabond/avalon-protocol/issues/70)) — not by
federation. Under federation, whether Game B can see a player's identity would
depend on which servers Game B's server peers with; that recreates the walled
gardens Avalon exists to remove. Under mirroring, a client does not pick "which
server to trust": any mirror that misrepresents the log is detectable, because
the log is self-verifying. Whether the log additionally anchors to, or becomes,
a chain is [#79](https://github.com/LunarVagabond/avalon-protocol/issues/79),
and does not change this.

## Discovery

A game developer should not need to know `postgres://...` or
`http://node-37.example.com`. The SDK should eventually resolve a node itself:

```rust
let avalon = Avalon::connect().await?;
```

Selection criteria over time: latency, geographic proximity, availability,
protocol version, advertised capabilities, health, settlement support, operator
preference, and, later, operator reputation. Version and capability negotiation
happen on connect; failover and retry live inside the SDK
([`./sdk.md`](./sdk.md)). Self-hosting stays possible without any central
registry — `connect_to(url)` remains for local development and private
deployments.

**Scenario K — a node disappears.** The SDK routes to another node advertising
the needed capabilities. Durable history is unaffected (it is mirrored);
presence for players on that node lapses until their next heartbeat
([`./presence.md`](./presence.md)); nothing a game had already verified becomes
unverifiable.

## Today in the repo

- Exactly one node type exists: `avalon-server` (`crates/server/src/main.rs`)
  running Gateway + Settlement (via `PostgresSettlementProvider`) in one
  process. No indexer implementation, no realtime service, no mirror.
- No discovery: `AvalonConfig { server_url, .. }` in `crates/sdk/src/lib.rs`
  takes a URL.
- No node-to-node protocol, no export format for the log, no capability
  advertisement endpoint.

## Decisions and tickets

- #70 mirrors of a public log, not federation
- #79 long-term settlement backend; #40 log design and mirror sync
- [#91](https://github.com/LunarVagabond/avalon-protocol/issues/91) SDK node
  discovery and capability negotiation
- [#72](https://github.com/LunarVagabond/avalon-protocol/issues/72) TLS before
  any non-local deployment
- [#78](https://github.com/LunarVagabond/avalon-protocol/issues/78) realtime is
  its own vertical and can become its own node
