# Avalon Protocol — Architecture

This is the normative architecture reference for Avalon. [`../stakeholders/Proposal.md`](../../../stakeholders/Proposal.md)
is the narrative design document and [`../WhyAvalon.md`](../../../WhyAvalon.md) is the
case for existence; this set states the invariants, the authority boundaries,
and what the code is held to. When the two disagree, this set wins and the
Proposal gets updated.

> **Don't build the universe. Build the infrastructure that lets others build
> worlds.** Avalon is the railroad between integrators, not an attempt to own every
> destination.

## Documents

| Topic | Document | One line |
|---|---|---|
| Overview | [overview.md](overview.md) | What Avalon is and is not; the three verticals; the workspace crates; phases |
| Identity | [identity.md](identity.md) | Self-owned, integrator-independent anchor; identity-controlled metadata |
| Integrator bindings | [bindings.md](bindings.md) | An identity's scoped participation in an integrator; characters stay integrator-owned |
| Achievements & attestations | [achievements-and-attestations.md](achievements-and-attestations.md) | An achievement is an issuer's signed claim, namespaced by issuer |
| Provenance | [provenance.md](provenance.md) | Who issued, where, when, who owns, revoked, valid, recognized |
| Trust model | [trust-model.md](trust-model.md) | Authentic, valid, and recognized are three different questions |
| Revocation | [revocation.md](revocation.md) | Revocation adds history; it never erases it |
| Integrators & issuers | [issuers.md](issuers.md) | Registration, issuer identity, key lifecycle, issuer status |
| Guilds | [guilds.md](guilds.md) | Network-level primitives; an integrator is a client of a guild, never its owner |
| Social graph | [social-graph.md](social-graph.md) | Friends persist across integrators; integrators never get the whole graph |
| Presence | [presence.md](presence.md) | The realtime vertical; ephemeral, never ledgered |
| Communication | [communication.md](communication.md) | DMs, voice, notifications: realtime infrastructure, not a Discord replacement |
| Synchronization | [synchronization.md](synchronization.md) | Offline/deferred SDK participation; an offline claim is never trusted like an online one |
| Cross-integrator events | [cross-integrator-events.md](cross-integrator-events.md) | Durable cross-integrator/special-event results as attestations — tournaments are one example |
| Integrator registry | [registry.md](registry.md) | Derived facts with explicit definitions; never a score |
| Protocol events | [protocol-events.md](protocol-events.md) | Durable event catalogue, versioning, history vs current state |
| Worked ledger example | [worked-ledger-example.md](worked-ledger-example.md) | One hypothetical user's ledger as real, ordered `ProtocolEvent` JSON — and what never appears on it |
| Identity aggregate view | [identity-aggregate-view.md](identity-aggregate-view.md) | One identity's full current-state JSON shape: Avalon-native layer-1 data vs. per-integrator layer-2 block space, and why one can't shape another's |
| Settlement | [settlement.md](settlement.md) | Batched commitments; transparency log on Postgres, no blockchain/validator consensus |
| Network trust anchors | [network-trust-anchors.md](network-trust-anchors.md) | Pinning `network_id` to the settlement operator's real key; `network_id` alone proves nothing |
| Query & indexing | [query-and-indexing.md](query-and-indexing.md) | Postgres is a projection, rebuildable from history |
| Nodes | [nodes.md](nodes.md) | Infrastructure providers, not authorities; roles, mirrors, discovery |
| Self-hosting | [self-hosting.md](self-hosting.md) | Running your own instance is supported, but it forks the network — mirroring the public log is not the same thing as a private `network_id` |
| Disaster recovery | [disaster-recovery.md](disaster-recovery.md) | Every Postgres disappears; what gets rebuilt, from what |
| Security model | [security-model.md](security-model.md) | Scoped authority; node authority; the three key domains; limitations |
| Privacy | [privacy.md](privacy.md) | Visibility is intentionally scoped; nothing is public because it can be |
| Scalability | [scalability.md](scalability.md) | 1,000 integrators × 100,000 identities, without becoming a gameplay bottleneck |
| Future layers | [future-layers.md](future-layers.md) | Portable assets and economy: later phases, not foundations |
| Integrator Space | [integrator-space.md](integrator-space.md) | Integrator-defined schemas, publication, versioning, and mappings |
| Distributed topology | [distributed-topology.md](distributed-topology.md) | Sharded settlement with no designated aggregator, interest-scoped realtime mesh instead of full broadcast |

These are the protocol-domain docs that live with `backend-server` (the crates
that ship as one `avalon-server` process: `protocol`, `chain`, `indexer`,
`server`). Two related topics live with the projects that consume this one
instead, since each is a separate deployable: the SDKs
([`../../sdks/architecture/sdk.md`](../../sdks/architecture/sdk.md) —
language-agnostic; exposes protocol capabilities, not infrastructure
topology) and the Hub
([`../../hub/architecture/hub.md`](https://github.com/avalon-initiative/avalon-hub/blob/main/docs/hub/architecture/hub.md) — a client
of the network, not the network).

## Invariants

These hold unless an explicit, documented decision changes them.

| Area | Invariant |
|---|---|
| Identity | Avalon identity is self-owned and integrator-independent. |
| Identity | An identity is a self-custodied keypair — a WebAuthn passkey for login, a separate Ed25519 key that signs the events it authors. No password, no shared secret. |
| Characters | Characters, and every integrator-defined attribute, belong to the integrator unless explicitly promoted. |
| Guilds | Avalon guilds are network-level, integrator-independent social primitives. |
| Achievements | Achievements are issuer attestations, not shared rows. |
| Provenance | Durable interoperable claims preserve provenance. |
| Trust | Authenticity, validity, and recognition are separate concepts. |
| Recognition | Consuming integrators choose what they recognize. No network-wide trust list. |
| History | Revocation adds history; it does not erase history. |
| Settlement | Settlement is not the general-purpose query database. |
| Settlement | Settlement is a public, verifiable, mirrorable log — never federation. |
| Settlement | No blockchain, no validator/BFT consensus; a transparency log on Postgres. No native currency or token at launch. |
| Settlement | Settlement storage is Postgres, permanently — not a per-node embedded store, since there's no validator/full-node model requiring one. |
| Trust anchors | `network_id` alone is never sufficient to trust a server; a client verifies STHs against the pinned key for the `network_id` claimed. See [network-trust-anchors.md](network-trust-anchors.md). |
| Batching | One event is never one settlement transaction. |
| Gameplay | Real-time gameplay stays game-side. |
| Query | Query databases are projections. |
| Rebuild | Promised-durable state is reconstructable from canonical history. |
| Presence | Realtime presence is ephemeral and never enters durable history. |
| Integrator Space | Schema publication and data exposure are independently authorized; historical data is read under the schema version it was recorded against, never reinterpreted. See [integrator-space.md](integrator-space.md). |
| Nodes | Nodes are infrastructure providers, not authorities. A node cannot fabricate an issuer's claim. |
| Self-hosting | A private instance (its own `network_id`) is cryptographically incapable of merging with the public network's log — running the code privately is supported, but it is a fork, not membership. See [self-hosting.md](self-hosting.md). |
| SDK | SDKs expose protocol capabilities, not infrastructure topology. |
| Hub | The Hub is a client of the network, not the network. |
| Registry | Network statistics inform decisions; they never determine trust. |
| Privacy | Network visibility is intentionally scoped. |
| Economy | Universal economic interoperability is not foundational. See [`../stakeholders/Proposal.md` §15](../../../stakeholders/Proposal.md#15-economy-and-currency). |
| Workspace | Domain functionality grows by module, not by new crate, unless a real compilation, ownership, or deployment boundary appears. |

Open design questions, deliberately not yet settled: identity recovery when
every passkey is lost beyond the guardian-based social recovery already
built; revocation mechanics beyond the append-only history model already in
place; offline operation capability classification, and the offline trust
model for client-recorded claims versus server-attested attestations.

## What survives an integrator's death

If Integrator A shuts down tomorrow:

| Survives (Avalon) | Disappears (Integrator A) |
|---|---|
| Avalon identity | Character |
| Friends | Level, stats, skill trees |
| Avalon guild membership and guild history | Quest progress |
| Achievements and attestations Integrator A issued | World position |
| Integrator event results | NPC relationships |
| Ownership and asset provenance (later phase) | Housing |
| Recognition history | Game-specific inventory |
| Integrator A's registration and key history | Game-specific economy |

Anything in the right column survives only if Integrator A explicitly promoted it into
durable protocol history while it was alive. This boundary is foundational; see
[bindings.md](bindings.md) and [disaster-recovery.md](disaster-recovery.md).

## Architecture tests

Ask these of any proposed change. The expected answer is in bold.

- Could `crates/protocol` still make sense if PostgreSQL disappeared? **Yes.**
- Could it still make sense if the settlement backend changed? **Yes.**
- Could it still make sense if HTTP were replaced? **Yes.**
- Could an integrator use Avalon without knowing the database topology? **Yes.**
- Could Avalon rebuild durable query state after losing PostgreSQL? **Yes** — real, tested projection-rebuild machinery backs this.
- Can a node operator fabricate an issuer claim? **No.**
- Can Avalon prove that an achievement is meaningful? **No.**
- Can a receiving integrator choose not to recognize a valid achievement? **Yes.**
- Does this change preserve integrator independence? Does it give Avalon authority it
  doesn't need? Does it force a game-specific concept into the protocol? Does it
  put hot gameplay on infrastructure that can't scale with gameplay? Is it
  something needed today rather than architecture for a hypothetical future?

## Scenarios

Concrete cases the design is tested against. Each names the document that
answers it.

| | Scenario | Answered in |
|---|---|---|
| A | An identity's owner enters a second integrator; it recognizes them without owning their identity | [bindings.md](bindings.md) |
| B | Integrator A issues `Dragon Slayer`; Integrator B verifies it | [achievements-and-attestations.md](achievements-and-attestations.md), [trust-model.md](trust-model.md) |
| C | Integrator A revokes it; history shows issued **and** revoked | [revocation.md](revocation.md) |
| D | Integrator C issues a trivial `Dragon Slayer`; Avalon preserves it, Integrator B rejects it | [trust-model.md](trust-model.md) |
| E | Integrator A rotates its signing key; old claims stay verifiable | [issuers.md](issuers.md) |
| F | Integrator A's key is compromised; new claims rejected, history intact | [issuers.md](issuers.md), [security-model.md](security-model.md) |
| G | Integrator A issues a cross-integrator event result (a championship, say) that Integrator B can verify | [cross-integrator-events.md](cross-integrator-events.md) |
| H | A guild exists outside any integrator, with members in three integrators at once | [guilds.md](guilds.md) |
| I | Integrator A shuts down; what survives | the table above |
| J | Every PostgreSQL database disappears; projections are rebuilt | [disaster-recovery.md](disaster-recovery.md) |
| K | A node disappears; SDKs route elsewhere | [nodes.md](nodes.md), [sdk architecture](../../sdks/architecture/sdk.md) |
| L | 1,000 integrators and 100M identities; Avalon is not a gameplay bottleneck | [scalability.md](scalability.md) |

## Implementation priority

When choosing what to build next, in this order: correct protocol semantics;
correct authority boundaries; durable history; verifiable attestations;
identity; guild and social primitives; developer experience; indexing and query;
the Hub; settlement optimization; portable assets; economy. Settlement
throughput is not optimized before the semantics it settles are right.

## Conventions for this directory

One file per topic. Each states its invariants up front and describes the
current model and implementation. Update the relevant file in the same
change as the code it describes; a doc that lags the code is a bug.
