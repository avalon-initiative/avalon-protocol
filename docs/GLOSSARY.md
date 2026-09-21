# Glossary

Avalon reuses ordinary words — identity, event, node, chain — for specific,
narrow meanings, and the gap between the ordinary meaning and the Avalon
meaning is the single biggest thing that slows a new reader down. This page
exists to close that gap fast: one definition per term, the distinction that
actually matters, and a link to the normative doc if you need the full
argument.

This is a companion to [`architecture/README.md`](projects/backend-server/architecture/README.md),
not a replacement for it — that document states invariants and links the
GitHub issues that decided them; this one just tells you what the words mean
so those documents are readable on a first pass. If a term's definition ever
drifts from its normative doc, the doc under `architecture/` wins.

New to the repo? Read [`README.md`](README.md) first, then come back here as
a reference while you read the architecture docs.

## Core nouns

| Term | Means | Does *not* mean | See also |
|---|---|---|---|
| **Identity** | A self-owned, integrator-independent keypair: a WebAuthn passkey for login plus a separate Ed25519 key that signs the events it authors. The one thing that survives any single integrator, server, or database disappearing. | A username/password account, or anything an integrator can define, rewrite, or own. | [identity.md](projects/backend-server/architecture/identity.md) |
| **Integrator** | This repo's general term for any independently operated game, app, or service that plugs into Avalon. Sovereign over its own world/data; Avalon never owns it. Older text and some code/tickets still say "game" — same concept, integrators aren't limited to games. | Avalon itself, or a node operator. | [overview.md](projects/backend-server/architecture/overview.md) |
| **Character** | Integrator-owned gameplay data (race, class, level, appearance, progression, inventory) that lives entirely in the integrator's own database. | An Avalon identity, or anything Avalon stores. | [identity.md](projects/backend-server/architecture/identity.md) |
| **Binding** (Integrator Profile) | The durable fact "this Avalon identity participates in this integrator" — nothing more. Scopes an integrator's authority to its own binding; it can never touch another integrator's. | The character data itself (that stays integrator-side). | [bindings.md](projects/backend-server/architecture/bindings.md) |
| **Issuer** | The durable, keyed identity an integrator (or other authorized party) signs attestations under. Has registered signing keys, a key history, and a status. A node operator is never an issuer — hosting infrastructure cannot sign claims on an integrator's behalf. | The node/server that transports or stores the claim. | [issuers.md](projects/backend-server/architecture/issuers.md) |
| **Achievement / Attestation** | A signed claim of the shape "Issuer X asserts Identity Y accomplished Z," with a timestamp and a schema. The durable fact is the *claim*, not a shared `user_id → achievement_id` row. | Proof the claim is meaningful — that's a separate question, see Recognition below. | [achievements-and-attestations.md](projects/backend-server/architecture/achievements-and-attestations.md) |
| **Guild** | A network-level social primitive that exists independently of any integrator: has members across many integrators at once, and survives any one integrator shutting down. An integrator is a client of a guild (can render/consume it), never its owner. | A guild/clan system owned and hosted by one game. | [guilds.md](projects/backend-server/architecture/guilds.md) |
| **Social graph** | An identity's friends, presence, and communication — relationships that persist across integrators and are never handed to an integrator wholesale, only through explicit capability grants and visibility scopes. | A per-integrator friends list. | [social-graph.md](projects/backend-server/architecture/social-graph.md) |
| **Integrator event** | An attestation for a durable, cross-integrator-relevant result (a tournament, a championship, a world-first) that an integrator runs and signs the outcome of. Only the result crosses into Avalon; live gameplay/brackets never do. | Live match state, spectator data, matchmaking. | [cross-integrator-events.md](projects/backend-server/architecture/cross-integrator-events.md) |
| **Integrator Space** | The design that lets an integrator publish a versioned, structurally described schema of its own data to Avalon (and control how much is exposed), without Avalon ever standardizing what that data *means*. | A shared/global data model integrators must conform to. | [integrator-space.md](projects/backend-server/architecture/integrator-space.md) |
| **Registry** | The network's intelligence layer about participating integrators/issuers — explicit-definition facts and aggregated statistics. | A ranking, a score, or a trust judgment — the registry never publishes those. | [registry.md](projects/backend-server/architecture/registry.md) |

## Trust and provenance

| Term | Means | Does *not* mean | See also |
|---|---|---|---|
| **Provenance** | The answer to who signed a claim, when, under which key, and whether it still stands — recorded as a first-class property of every durable claim, surviving the death of whoever produced it. | Meaning or trustworthiness of the claim. | [provenance.md](projects/backend-server/architecture/provenance.md) |
| **Authentic** | The claim's signature verifies against the issuer's registered key. A cryptographic fact, checkable by anyone. | Valid or Recognized — see below; a claim can be authentic and still trivial or revoked. | [trust-model.md](projects/backend-server/architecture/trust-model.md) |
| **Valid** | The claim hasn't been revoked and its issuer wasn't suspended/revoked at time of issuance. | Recognized — a valid claim can still be one a receiving integrator chooses to ignore. | [trust-model.md](projects/backend-server/architecture/trust-model.md) |
| **Recognized** | Whether a *specific receiving* integrator chooses to honor a claim. Entirely contextual — decided per-integrator, never by the network. | A network-wide trust list; Avalon has none. | [trust-model.md](projects/backend-server/architecture/trust-model.md) |
| **Revocation** | An appended fact — "Integrator A later revoked claim X" — layered on top of, never replacing, the original "issued" fact. Both stay visible forever. | Deletion. Revoking an issuer doesn't erase what it ever signed either. | [revocation.md](projects/backend-server/architecture/revocation.md) |

## Settlement, history, and infrastructure

| Term | Means | Does *not* mean | See also |
|---|---|---|---|
| **Protocol event** | A durable fact Avalon considers part of its history (identity created, friend accepted, achievement issued, ...). Append-only — a correction is a new event, never an edit. Ordinary gameplay is never a protocol event. | Every action an integrator takes. | [protocol-events.md](projects/backend-server/architecture/protocol-events.md) |
| **Settlement / Ledger / Chain crate** | The durable-history vertical: a hash-chained, append-only, publicly verifiable log (currently Postgres-backed) that commits protocol events in batches. Not a blockchain — no validator set, no consensus, because nothing written to it is ever contested. The `chain` crate is its current implementation. | A query database, or a cryptocurrency chain. | [settlement.md](projects/backend-server/architecture/settlement.md) |
| **Signed Tree Head (STH)** | A signed, periodically-published root over the ledger's current hash-chain state, used by clients/mirrors to verify the log hasn't been tampered with, served at `GET /ledger/sth/latest`. | A block on a blockchain. | [network-trust-anchors.md](projects/backend-server/architecture/network-trust-anchors.md) |
| **`network_id`** | A plain string label for a deployment (e.g. `avalon-dev-local`, `avalon-mainnet-1`) with **zero cryptographic authority on its own** — anyone can stand up a server and claim the same string. Real trust comes from pinning that string to the deployment's actual Ed25519 settlement-verify key (see `docs/trusted-networks.json`). | A security boundary by itself. | [network-trust-anchors.md](projects/backend-server/architecture/network-trust-anchors.md) |
| **Indexer** | The fast-read query layer — a Postgres projection rebuildable from durable protocol event history. Kept deliberately separate from settlement (settlement vs. querying are different verticals). | The source of truth. | [query-and-indexing.md](projects/backend-server/architecture/query-and-indexing.md) |
| **Outbox pattern** | The write pattern that makes a domain write (e.g. creating an identity) and its ledger entry commit atomically in one Postgres transaction, so a crash can never leave one without the other. | An async queue or message broker. | [disaster-recovery.md](projects/backend-server/architecture/disaster-recovery.md) |
| **Presence** | Realtime online/offline/activity state. Ephemeral by design — never enters durable protocol history; losing it just means identities show offline until their next heartbeat. | A durable history of when someone was online. | [presence.md](projects/backend-server/architecture/presence.md) |
| **Node** | Infrastructure that transports, indexes, settles, and/or serves protocol data — an infrastructure *provider*, never an authority. A node cannot fabricate an issuer's claim or forge a signature. Capabilities (Settlement, Indexer, Gateway, ...) are roles an operator opts into, not mandatory separate binaries. | A validator with governance power, or the issuer of anything it transports. | [nodes.md](projects/backend-server/architecture/nodes.md) |
| **Mirror** | A node that syncs and re-serves the *same* `network_id`'s public log — participation in the same network, not a fork of it. | Self-hosting a private instance (different `network_id` — see below). | [nodes.md](projects/backend-server/architecture/nodes.md), [self-hosting.md](projects/backend-server/architecture/self-hosting.md) |
| **Self-hosting** | Running the Avalon code under your *own* `network_id`. Fully supported, but cryptographically incapable of merging back with the public network's log later — it's a fork, not membership. | Mirroring (see above). | [self-hosting.md](projects/backend-server/architecture/self-hosting.md) |

## Identity mechanics

| Term | Means | Does *not* mean | See also |
|---|---|---|---|
| **Passkey** | A WebAuthn credential used to log in to an Avalon identity. An identity can register multiple passkeys/devices. | The signing key (see Ed25519 signing key). | [identity.md](projects/backend-server/architecture/identity.md) |
| **Ed25519 signing key** | The separate key (not the login passkey) that signs the protocol events an identity authors. | The login credential. | [identity.md](projects/backend-server/architecture/identity.md) |
| **Social recovery (M-of-N guardians)** | Recovering an identity when passkeys are lost via approval from M of N designated guardians, rather than a password reset. | A centrally-held recovery backdoor. | `docs/projects/backend-server/architecture/identity.md`, issue #201 |
| **Cross-device pairing** | Registering an additional device/passkey to an existing identity without starting over. | Creating a second identity. | `docs/projects/backend-server/architecture/identity.md`, issue #307 |
| **Capability (grant)** | An explicit, scoped permission an identity grants an integrator (e.g. "read my friends list") — the mechanism behind least-privilege access. Every SDK method checks its own required grant. | Blanket access to an identity's whole history. | [sdk.md](projects/sdks/architecture/sdk.md), [security-model.md](projects/backend-server/architecture/security-model.md) |

## Workspace and code map

| Term | Means | See also |
|---|---|---|
| **`protocol` crate** | Pure domain types/traits (identity, guilds, achievements, events). No I/O. | [overview.md](projects/backend-server/architecture/overview.md) |
| **`chain` crate** | The `SettlementProvider` trait plus the Postgres-backed hash-chained ledger implementation. | [settlement.md](projects/backend-server/architecture/settlement.md) |
| **`indexer` crate** | The fast-read query layer, rebuildable from durable protocol events. | [query-and-indexing.md](projects/backend-server/architecture/query-and-indexing.md) |
| **`server` crate** | The one network-facing API/auth/realtime service every client (Hub, mobile-hub, integrators) talks to. | [overview.md](projects/backend-server/architecture/overview.md) |
| **`sdk` crate** | The Rust reference SDK. | [sdk.md](projects/sdks/architecture/sdk.md) |
| **`cli` crate** | Local dev/ops tooling — the `avalon` binary (`create-identity`, `inspect-ledger`, ...). | root `README.md` |
| **Hub** (`apps/hub`) | The Vue3 web client — a user's first doorway into Avalon with no integrator open. A client of the network like any other, not the network itself, and has no backend of its own. | [hub.md](projects/hub/architecture/hub.md) |
| **`apps/mobile-hub`** | A Tauri desktop/mobile shell around the same Hub UI, for guild/friend presence without a game client open. | root `README.md` |
| **`packages/ui`** | The shared Vue3 component library (`@avalon/ui`) used by both Hub apps. | root `README.md` |
| **`bindings/csharp`** | The flagship external SDK for game developers, Unity-targeted (netstandard2.1). Rust SDK is the reference implementation; C# is the priority developer-facing surface. | root `README.md` |

## Where a term came from

Most of these definitions are direct paraphrases of the bolded opening claims
in each `docs/projects/backend-server/architecture/*.md` file — that's deliberate, so this page never
becomes a second source of truth to keep in sync. If a term you need isn't
here, check the matching `docs/projects/backend-server/architecture/` doc first; if it's genuinely
missing from both, that's worth a PR to this file.
