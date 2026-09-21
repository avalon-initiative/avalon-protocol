# Avalon Protocol

Avalon Protocol is an open, Rust-based interoperability layer for independent
games, apps, and services: one persistent identity, one social graph, that a
user carries between the games, apps, and services that would otherwise treat
every login as a stranger.

Every major platform already solved this for the web: sign in once, and that
identity carries weight across dozens of unrelated apps — the browser or
platform vouches for who you are everywhere you go, so nobody has to rebuild
their identity from scratch at every login screen. Gaming never got this. Your
online presence — who your friends are, what community you're part of, the
journey you've built — tends to be the same person across every game you
play, yet today it resets to zero at each one, trapped in whichever studio's
database happens to run that particular title. There's no reason a friendship
or a guild built in a shooter should be invisible the moment you log into a
survival game instead. Avalon is that missing layer for games: the same
identity, friends, and history carried with you from one game to the next —
owned by the user, not any single game.

> **Games are experiences. Your identity, friends, guilds, achievements, and
> history belong to you.**

A user has one Avalon identity. That identity can have completely different,
unrelated characters in different games — Avalon never dictates a character model.
Friends, guilds, and achievements are network-level concepts a game opts into, not
things a game is required to expose or trust blindly. A game stays fully sovereign
over its own world, economy, and rules; Avalon only provides the connective
infrastructure between games, not a platform that owns them.

The internet was built to connect people to each other. Identity and social
connection never became part of that shared foundation the way addressing and
routing did, so every app and platform built its own incompatible version on
top instead — and the network built to connect everyone ended up full of
places that each make you start over. Avalon is an attempt to put that layer
where it always should have been: not owned by any one platform, and not
rebuilt from scratch by every game that needs it. See
[Why Avalon](docs/WhyAvalon.md) for the full argument.

## We are the Avalon Initiative

Not one studio, and not a platform selling access to the games that use it.
Run a node, mirror the log, build against the SDK, or ship a game that plugs
in — however you touch this network, you're not a customer of it, you're part
of it. An open protocol only stays open as long as more than one party is
actually running it; every operator, integrator, and contributor here is
that party, not just whoever wrote the reference implementation first.

## Why Avalon

- **Identity is separate from characters.** One persistent identity, any number of
  unrelated characters across games — see [ADR: Identity Is Separate From Game Characters](https://github.com/LunarVagabond/avalon-protocol/issues/67).
- **Interoperability is opt-in.** A game chooses which Avalon capabilities it wants
  (identity, guilds, achievements, ...) and which other issuers' attestations it
  trusts. Nothing is forced.
- **Least privilege by default.** A game receives only the capabilities a user
  has explicitly granted — never blanket access to an identity's whole history.
- **History is verifiable, not just shared.** Achievements are signed attestations
  a receiving game can independently verify and decide whether to trust — not
  arbitrary rows in someone else's database.
- **Settlement is a public transparency log, not federation or blockchain consensus.** Milestone 1 is a signed, append-only ledger. Long-term, durable facts are independently verifiable and mirrorable by anyone — not gated behind servers whitelisting each other, and not requiring mining/consensus to referee a scarcity problem Avalon doesn't have — see [ADR: Attestations Before Blockchain](https://github.com/LunarVagabond/avalon-protocol/issues/68) and [ADR: Settlement Is a Public Transparency Log](https://github.com/LunarVagabond/avalon-protocol/issues/70).
- **Don't build the universe.** Avalon is the railroad between games, not another
  platform trying to own every destination.

## Closing the gap

Tools already exist that solve part of this problem, each in its own way.
Discord is the clearest example: one identity, one friends list, presence and
communities that already span every game you play — proof the demand this
README opens with is real, not hypothetical. What none of them give you is
anything a game can actually build on: there's no achievement a game can
issue and another can independently verify, no way for a game to trust a
claim made outside its own database, and no social graph a user actually
owns in a portable sense — it belongs to whichever platform happens to be
hosting it. Avalon Protocol closes that gap. It isn't a competitor to
Discord, Slack, or anything like them — it's the open identity and social
layer underneath, that any of them could plug into as a client, the same way
a game or the Hub app can.

## Status

The core vertical slice is real and working end to end against a live
Postgres instance, not scaffolding. Identity and auth: a self-custodied
keypair — a WebAuthn passkey for login plus a separate Ed25519 key that
signs the events an identity authors — with multi-device registration,
guardian-based social recovery, and cross-device pairing all real and
live-tested, wired through `avalon create-identity`, the Rust SDK, and the
C# SDK. The chain crate has a real hash-chained, Merkle-rooted Postgres
ledger with Signed Tree Heads and mirror-facing proof/sync endpoints
(`avalon inspect-ledger`); identity, guild, and achievement writes are
atomic with their ledger entry via an outbox pattern. The social graph
(friends, blocks, presence, scoped discovery) and guilds (roles with
per-resource permission overrides, membership, channels, chat, events with
RSVP, discovery) are built out with real server endpoints and a working Hub
UI. Achievements/attestations are wired end to end — a category-driven
claim vocabulary, two-tier root/operational issuer keys, signed issuance,
authenticity/validity/recognition kept as separate questions, signed
append-only revocation history — through both SDKs and a Hub achievements
view. See [`docs/architecture/`](docs/projects/backend-server/architecture/) for the current state
of each area, one file per topic.

## Running locally

```bash
docker compose up -d && cp .env.compose.example .env
make migrate && make start
make create-identity && make inspect-ledger
```

See [`docs/maintainers/local-development.md`](docs/maintainers/local-development.md)
for the full setup path (prerequisites, the Hub web client, Storybook, the
C# SDK, resetting the database, and running the live test suite).

## Repository structure

```text
crates/
  protocol/   pure domain types & traits — identity, guilds, achievements, events
  chain/      SettlementProvider trait + ledger implementation (not a blockchain yet)
  indexer/    fast-read query layer, rebuildable from durable protocol events
  server/     the network-facing API/auth service every client talks to
  sdk/        Rust reference SDK
  cli/        local dev/ops tooling (`avalon` binary)

apps/
  hub/          web client — Vue3, the first doorway into Avalon
  mobile-hub/   Tauri companion app (desktop/mobile), same UI as hub

packages/
  ui/         shared Vue3 component library used by both hub apps (Storybook)

bindings/
  csharp/     flagship external SDK for game developers (Unity-targeted)

docs/
  README.md      doc-set map: which directory is for you, suggested reading order
  GLOSSARY.md    Avalon's vocabulary — start here if the terminology is the blocker
  stakeholders/
    Proposal.md  the living design document (narrative)
  WhyAvalon.md   the case for why this needs to exist
  architecture/  the normative architecture reference, one file per topic
  users/         docs for people using games/apps/services that integrate Avalon
  developers/    docs for developers integrating the SDKs
  maintainers/   docs for contributors to this repo
  hosters/       docs for standing up and operating an avalon-server node
  stakeholders/  docs for people evaluating Avalon from the outside
```

Architecture decisions are tracked as closed GitHub issues labeled
`architecture-decision-record`, not as files in this repo — see
[decided](https://github.com/LunarVagabond/avalon-protocol/issues?q=is%3Aissue+label%3Aarchitecture-decision-record)
and [still-open](https://github.com/LunarVagabond/avalon-protocol/issues?q=is%3Aissue+label%3Adecision+is%3Aopen)
decisions.

## Trusted networks

![trust anchors](https://img.shields.io/badge/trust--anchors-1%20network%20pinned-blue)

`network_id` (e.g. `avalon-dev-local`, `avalon-mainnet-1`) is a plain string
with zero cryptographic authority on its own — anyone can stand up a server
and claim the same one. The table below is Avalon's actual root of trust: it
pins each known network's `network_id` to the settlement operator's real
Ed25519 public key, so a client can verify a server's Signed Tree Heads
(`GET /ledger/sth/latest`) instead of trusting the name alone. This table is
rendered from [`docs/trusted-networks.json`](docs/trusted-networks.json), the
single canonical copy — not a hand-maintained duplicate, and a Hub test fails
if the two ever drift. See
[`docs/architecture/network-trust-anchors.md`](docs/projects/backend-server/architecture/network-trust-anchors.md)
for the full model, how the Hub enforces it, and what this deliberately does
not solve (a compromised maintainer publishing a bad key here is a
governance problem, not one client-side pinning can fix).

| Label | `network_id` | STH verify key (Ed25519, hex) |
|---|---|---|
| `avalon-dev-local` *(local-dev — no real deployment yet, see notes below)* | `avalon-dev-local` | `bbcb11ead3d7c68d58ddf0f923df6e7e8347a4341e93eb7b07c3c7a2622accd7` |

This repo has no publicly deployed Avalon network yet, so the entry above is
a template: a real, freely-generated Ed25519 key with no server behind it,
checked in so the pinning mechanism is exercised end to end rather than left
as an unfilled stub. Adding a real network is a normal reviewed PR against
`docs/trusted-networks.json`: append its `network_id` and the actual hex from
that deployment's `AVALON_SETTLEMENT_VERIFY_KEY` (see `.env.example`). Three
deployment tiers are supported — `avalon-dev-<name>` (single-node),
`avalon-int-<name>` (a 1-5 node interconnected test bed for verifying changes
integrate before mainnet), and `avalon-mainnet-N` (the real, independently
growing/shrinking validator set) — see
[`docs/architecture/network-trust-anchors.md`](docs/projects/backend-server/architecture/network-trust-anchors.md#the-trust-anchor-list)
for what each tier's `environment` value means.

Avalon Hub bundles this same list at build time and always shows which
pinned network the current session is connected to, flagging a mismatch or
an unpinned network rather than trusting it silently — see
`apps/hub/src/network/`.

## Learn more

| Design | Decisions | Process |
|---|---|---|
| [Doc map](docs/README.md) · [Glossary](docs/GLOSSARY.md) · [Proposal](docs/stakeholders/Proposal.md) · [Architecture](docs/projects/backend-server/architecture/README.md) · [Why Avalon](docs/WhyAvalon.md) | [Decided](https://github.com/LunarVagabond/avalon-protocol/issues?q=is%3Aissue+label%3Aarchitecture-decision-record) · [Open](https://github.com/LunarVagabond/avalon-protocol/issues?q=is%3Aissue+label%3Adecision+is%3Aopen) | [Contributing](.github/CONTRIBUTING.md) |

## Contributing

This repo is private and pre-release; see [CONTRIBUTING.md](.github/CONTRIBUTING.md) for the
workflow once it opens up.

## Support the project

Avalon stays useful because people use it, report what's broken, and help
build it out — that's worth as much as the financial side. If you enjoy the
protocol and want to support development directly, buying a coffee is
appreciated but entirely optional:

<a href="https://www.buymeacoffee.com/lunarvagabond" target="_blank"><img src="https://cdn.buymeacoffee.com/buttons/v2/default-blue.png" alt="Buy Me a Coffee" style="height: 60px !important;width: 217px !important;" ></a>

Code, docs, bug reports, and testing feedback all help just as much.

## License

Apache-2.0 — see [LICENSE](LICENSE).
