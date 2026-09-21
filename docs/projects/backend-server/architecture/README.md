# Avalon Protocol — Architecture

This is the normative architecture reference for Avalon. [`../stakeholders/Proposal.md`](../../../stakeholders/Proposal.md)
is the narrative design document and [`../WhyAvalon.md`](../../../WhyAvalon.md) is the
case for existence; this set states the invariants, the authority boundaries,
and what the code is held to. When the two disagree, this set wins and the
Proposal gets updated.

Decisions that constrain the architecture are recorded as closed GitHub issues
labeled [`architecture-decision-record`](https://github.com/LunarVagabond/avalon-protocol/issues?q=is%3Aissue+label%3Aarchitecture-decision-record).
Questions still being decided are open issues labeled
[`decision`](https://github.com/LunarVagabond/avalon-protocol/issues?q=is%3Aissue+label%3Adecision+is%3Aopen).
Each document below links the ones that govern it.

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
| Integrator Space | [integrator-space.md](integrator-space.md) | Integrator-defined schemas, publication, versioning, and mappings; not built yet — the design a decision has to close before it starts |
| Distributed topology | [distributed-topology.md](distributed-topology.md) | Target shape, not today's reality: sharded settlement with no designated aggregator, interest-scoped realtime mesh instead of full broadcast |

These are the protocol-domain docs that live with `backend-server` (the crates
that ship as one `avalon-server` process: `protocol`, `chain`, `indexer`,
`server`). Two related topics live with the projects that consume this one
instead, since each is a separate deployable: the Rust SDK
([`../../rust-sdk/architecture/sdk.md`](../../rust-sdk/architecture/sdk.md) —
exposes protocol capabilities, not infrastructure topology) and the Hub
([`../../hub/architecture/hub.md`](../../hub/architecture/hub.md) — a client
of the network, not the network).

## Invariants

These hold unless an explicit decision changes them. Each links the record that
established it.

| Area | Invariant | Record |
|---|---|---|
| Identity | Avalon identity is self-owned and integrator-independent. | [#67](https://github.com/LunarVagabond/avalon-protocol/issues/67) |
| Identity | An identity is a self-custodied keypair — a WebAuthn passkey for login, a separate Ed25519 key that signs the events it authors. No password, no shared secret. | [#73](https://github.com/LunarVagabond/avalon-protocol/issues/73) |
| Characters | Characters, and every integrator-defined attribute, belong to the integrator unless explicitly promoted. | [#67](https://github.com/LunarVagabond/avalon-protocol/issues/67) |
| Guilds | Avalon guilds are network-level, integrator-independent social primitives. | [#74](https://github.com/LunarVagabond/avalon-protocol/issues/74) |
| Achievements | Achievements are issuer attestations, not shared rows. | [#76](https://github.com/LunarVagabond/avalon-protocol/issues/76) |
| Provenance | Durable interoperable claims preserve provenance. | [#75](https://github.com/LunarVagabond/avalon-protocol/issues/75), [#76](https://github.com/LunarVagabond/avalon-protocol/issues/76) |
| Trust | Authenticity, validity, and recognition are separate concepts. | [#76](https://github.com/LunarVagabond/avalon-protocol/issues/76) |
| Recognition | Consuming integrators choose what they recognize. No network-wide trust list. | [#76](https://github.com/LunarVagabond/avalon-protocol/issues/76) |
| History | Revocation adds history; it does not erase history. | [#75](https://github.com/LunarVagabond/avalon-protocol/issues/75) |
| Settlement | Settlement is not the general-purpose query database. | [#68](https://github.com/LunarVagabond/avalon-protocol/issues/68), [#70](https://github.com/LunarVagabond/avalon-protocol/issues/70) |
| Settlement | Settlement is a public, verifiable, mirrorable log — never federation. | [#70](https://github.com/LunarVagabond/avalon-protocol/issues/70) |
| Settlement | No blockchain, no validator/BFT consensus; a transparency log on Postgres. No native currency or token at launch. | [ADR #186](https://github.com/LunarVagabond/avalon-protocol/issues/186), [#79](https://github.com/LunarVagabond/avalon-protocol/issues/79) |
| Settlement | Settlement storage is Postgres, permanently — not a per-node embedded store, since there's no validator/full-node model requiring one. | [ADR #186](https://github.com/LunarVagabond/avalon-protocol/issues/186) |
| Trust anchors | `network_id` alone is never sufficient to trust a server; a client verifies STHs against the pinned key for the `network_id` claimed. | [#232](https://github.com/LunarVagabond/avalon-protocol/issues/232), [network-trust-anchors.md](network-trust-anchors.md) |
| Batching | One event is never one settlement transaction. | [#68](https://github.com/LunarVagabond/avalon-protocol/issues/68) |
| Gameplay | Real-time gameplay stays game-side. | [#68](https://github.com/LunarVagabond/avalon-protocol/issues/68) |
| Query | Query databases are projections. | [#75](https://github.com/LunarVagabond/avalon-protocol/issues/75) |
| Rebuild | Promised-durable state is reconstructable from canonical history. | [#75](https://github.com/LunarVagabond/avalon-protocol/issues/75) |
| Presence | Realtime presence is ephemeral and never enters durable history. | [#78](https://github.com/LunarVagabond/avalon-protocol/issues/78) |
| Integrator Space | Schema publication and data exposure are independently authorized; historical data is read under the schema version it was recorded against, never reinterpreted. | [integrator-space.md](integrator-space.md), [#181](https://github.com/LunarVagabond/avalon-protocol/issues/181) |
| Nodes | Nodes are infrastructure providers, not authorities. A node cannot fabricate an issuer's claim. | [#70](https://github.com/LunarVagabond/avalon-protocol/issues/70) |
| Self-hosting | A private instance (its own `network_id`) is cryptographically incapable of merging with the public network's log — running the code privately is supported, but it is a fork, not membership. | [#173](https://github.com/LunarVagabond/avalon-protocol/issues/173), [self-hosting.md](self-hosting.md) |
| SDK | SDKs expose protocol capabilities, not infrastructure topology. | [#69](https://github.com/LunarVagabond/avalon-protocol/issues/69) |
| Hub | The Hub is a client of the network, not the network. | [#77](https://github.com/LunarVagabond/avalon-protocol/issues/77) |
| Registry | Network statistics inform decisions; they never determine trust. | [#76](https://github.com/LunarVagabond/avalon-protocol/issues/76) |
| Privacy | Network visibility is intentionally scoped. | [#78](https://github.com/LunarVagabond/avalon-protocol/issues/78), [#87](https://github.com/LunarVagabond/avalon-protocol/issues/87) |
| Economy | Universal economic interoperability is not foundational. | [`../stakeholders/Proposal.md` §15](../../../stakeholders/Proposal.md#15-economy-and-currency) |
| Workspace | Six domain crates plus one proc-macro support crate (`schema-derive`); new domains are modules of `protocol`, not new crates. | [#69](https://github.com/LunarVagabond/avalon-protocol/issues/69) |

Still open, and deliberately so:

| Question | Issue |
|---|---|
| Transparency log design (hash structure, signed tree heads, mirror sync) — no validator/consensus/storage-engine design needed, decided closed per [ADR #186](https://github.com/LunarVagabond/avalon-protocol/issues/186) | [#40](https://github.com/LunarVagabond/avalon-protocol/issues/40) |
| Identity recovery when every passkey is lost | [#99](https://github.com/LunarVagabond/avalon-protocol/issues/99) |
| Issuer signing keys and key lifecycle | [#80](https://github.com/LunarVagabond/avalon-protocol/issues/80) |
| Revocation mechanics | [#81](https://github.com/LunarVagabond/avalon-protocol/issues/81) |
| Ledger entry / tree-head signing (the operator's key) | [#39](https://github.com/LunarVagabond/avalon-protocol/issues/39) |
| Offline operation capability classification | [#109](https://github.com/LunarVagabond/avalon-protocol/issues/109) |
| Offline trust model: client-recorded claims vs server-attested attestations | [#112](https://github.com/LunarVagabond/avalon-protocol/issues/112) |

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
- Could Avalon rebuild durable query state after losing PostgreSQL? **In principle, yes** — and [#43](https://github.com/LunarVagabond/avalon-protocol/issues/43) proves it.
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
| K | A node disappears; SDKs route elsewhere | [nodes.md](nodes.md), [rust-sdk architecture](../../rust-sdk/architecture/sdk.md) |
| L | 1,000 integrators and 100M identities; Avalon is not a gameplay bottleneck | [scalability.md](scalability.md) |

## Implementation priority

When choosing what to build next, in this order: correct protocol semantics;
correct authority boundaries; durable history; verifiable attestations;
identity; guild and social primitives; developer experience; indexing and query;
the Hub; settlement optimization; portable assets; economy. Settlement
throughput is not optimized before the semantics it settles are right.

## Conventions for this directory

One file per topic. Each states its invariants up front, describes the model,
ends with "Today in the repo" (what actually exists, with paths) and "Decisions
and tickets" (the issues that govern it). Update the relevant file in the same
PR as the code it describes; a doc that lags the code is a bug.
