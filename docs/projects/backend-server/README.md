# backend-server

## What this is, in plain language

Imagine every video game and app had its own separate passport office: your
friends list, your achievements, your guild — none of it carries over when
you close one game and open another. `backend-server` is the office that
lets a bunch of *different, independently run* games and apps agree to
recognize the same passport. You get one identity, one friends list, and a
record of what you've done, that follows you between them — without any
single game controlling it, and without you having to trust one company
with all of it.

It doesn't run your game. It doesn't store your character, your inventory,
or anything that happens mid-match — that all stays with the game, forever,
because the game's world is the game's business. It only keeps the small
set of things that are supposed to survive even if one particular game shuts
down tomorrow: who you are, who your friends are, what guild you're in, and
what you've achieved. See [`avalon-docs: getting-started/why-avalon.md`](https://github.com/avalon-initiative/avalon-docs/blob/main/getting-started/why-avalon.md) for the
fuller case for why this needs to exist as its own thing at all.

## What this is, technically (the meta)

`backend-server` is four Rust crates that build into one binary,
`avalon-server`:

| Crate | Job | May know about | Must not know about |
|---|---|---|---|
| `protocol` | Pure domain types and traits — identity, guilds, achievements, permissions, events. No I/O. | domain types, ids, events, traits | Postgres, any chain, HTTP, the server, any node implementation |
| `chain` | Settlement: the `SettlementProvider` trait plus a real hash-chained, Merkle-rooted Postgres ledger with Signed Tree Heads. | commitments, verification, the ledger/log | general domain semantics (those live in `protocol`) |
| `indexer` | The fast-read query layer — projections rebuildable from durable protocol events, never a second source of truth. | consuming events, projections, read models | redefining what an event means |
| `server` | Composes all three plus realtime/presence into the actual network-facing API. | everything | being reached around by clients — Hub, integrators use the API/SDK, not this crate directly |

**Why these four live in one project folder, not four:** they compile into
one binary today, and — per the current repo-split plan — they're intended
to move together if this repository ever splits into several, as the "core"
repo everything else (SDKs, Hub, CLI) depends on rather than lives inside.

**One binary, but not necessarily one process.** The same compiled binary
can be started multiple times with different `AVALON_NODE_ROLES`
(`settlement`, `indexer`, `realtime`, `gateway`, or `combined`), and those
processes talk to each other over an internal RPC protocol. So a hosting
setup can genuinely be several specialized processes, not just a single
monolith with room to split later — see
[`avalon-docs: architecture/nodes/README.md`](https://github.com/avalon-initiative/avalon-docs/blob/main/architecture/nodes/README.md) for the roles and
[`avalon-docs: architecture/overview.md`](https://github.com/avalon-initiative/avalon-docs/blob/main/architecture/overview.md) for the "three
verticals" model those roles map onto.

**Status:** the most mature project in this repository. Real,
live-tested end to end against Postgres, not scaffolding — identity/auth
(passkey + Ed25519 signing key, multi-device, social recovery, cross-device
pairing), the social graph (friends, blocks, presence, discovery), guilds
(roles, permissions, channels, chat, events), achievements/attestations
(issuer keys, signed issuance, authenticity/validity/recognition kept
separate, revocation history), and a real hash-chained Merkle ledger with
mirror sync. See [`avalon-docs: architecture/overview.md`](https://github.com/avalon-initiative/avalon-docs/blob/main/architecture/overview.md)'s
implementation notes for the current detail.

## Find your door

| I am... | Start here |
|---|---|
| Curious what this whole project is for, no jargon | You just read it — see also [`avalon-docs: getting-started/why-avalon.md`](https://github.com/avalon-initiative/avalon-docs/blob/main/getting-started/why-avalon.md) and [`avalon-docs: reference/glossary.md`](https://github.com/avalon-initiative/avalon-docs/blob/main/reference/glossary.md) if a term trips you up |
| Standing up my own `avalon-server` node | [`for-hosters/README.md`](for-hosters/README.md) |
| Building a game/app/service that talks to this backend | Start with the SDK for your language — [`avalon-docs: sdk/README.md`](https://github.com/avalon-initiative/avalon-docs/blob/main/sdk/README.md) — this project's own [`avalon-docs: architecture/README.md`](https://github.com/avalon-initiative/avalon-docs/blob/main/architecture/README.md) is the reference for what the SDKs are wrapping |
| Contributing code to `protocol`/`chain`/`indexer`/`server` | [`for-maintainers/`](for-maintainers) (backend-specific ops docs) plus the repo-wide [`../../maintainers/README.md`](../../maintainers/README.md) |
| Deciding whether to back, partner with, or build on Avalon | [`avalon-docs: getting-started/users.md`](https://github.com/avalon-initiative/avalon-docs/blob/main/getting-started/users.md) and [`avalon-docs: architecture/design-proposal.md`](https://github.com/avalon-initiative/avalon-docs/blob/main/architecture/design-proposal.md) |
| Reading the normative design — invariants, authority boundaries, what's actually built | [`avalon-docs: architecture/README.md`](https://github.com/avalon-initiative/avalon-docs/blob/main/architecture/README.md) |

## In this folder

- [`avalon-docs: architecture/README.md`](https://github.com/avalon-initiative/avalon-docs/blob/main/architecture/README.md) — the normative reference. One
  file per protocol-domain topic (identity, guilds, achievements, trust
  model, settlement, nodes, ...), each describing the current model and
  implementation. This is also where the whole-system map lives
  ([`avalon-docs: architecture/overview.md`](https://github.com/avalon-initiative/avalon-docs/blob/main/architecture/overview.md)), since the
  concepts it maps are defined by these four crates.
- [`for-hosters/`](for-hosters/README.md) — running your own `avalon-server`
  node: quickstart, TLS/production deployment, upgrading a running node.
- [`for-maintainers/`](for-maintainers) — backend-specific operational docs:
  cutting a release, testing a local multi-node network, rotating the
  settlement signing key, responding to equivocation, the milestone-1
  end-to-end walkthrough.

## Related projects

- [`avalon-docs: sdk/README.md`](https://github.com/avalon-initiative/avalon-docs/blob/main/sdk/README.md) — every official SDK, and how a game or
  app actually talks to this backend; none of them ship as part of
  `avalon-server` itself.
- [`../cli/`](../cli/README.md) (`avalon`) is a standalone dev/ops tool that
  talks to this backend over the same API surface a client would.
- [`../hub/`](https://github.com/avalon-initiative/avalon-hub/blob/main/docs/hub/README.md) is a *client* of this backend, same as any
  integrator — it has no backend of its own.
