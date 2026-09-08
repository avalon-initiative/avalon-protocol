# Avalon Protocol

**Status:** early. The Rust workspace (`protocol`, `chain`, `indexer`, `server`,
`sdk`, `cli`), the web/mobile Hub apps, and the C# SDK skeleton all exist and
build. Identity and auth work end to end against a live Postgres: an identity
is a self-custodied keypair, not a password — a WebAuthn passkey for login and
a separate Ed25519 key that signs the events an identity authors, so a hosted
node can't fabricate one (`make create-identity`, the Rust SDK's
`authenticate()`). Every identity creation lands in a hash-chained ledger,
atomically with the identity itself via an outbox, inspectable with
`make inspect-ledger` / `make outbox-status`. Social, guilds, achievements,
permissions, the indexer, and the Hub UIs are still scaffolding. See [`docs/stakeholders/Proposal.md`](docs/stakeholders/Proposal.md)
for the full design, [`docs/architecture/`](docs/architecture/README.md) for the
normative architecture and its invariants, and
[`docs/WhyAvalon.md`](docs/WhyAvalon.md) for the case for why this needs to exist.

Avalon Protocol is an open, Rust-based interoperability layer for independent games:
one persistent player identity, one social graph, that a player carries between
games that would otherwise treat every login as a stranger.

> **Games are experiences. Your identity, friends, guilds, achievements, and
> history belong to you.**

A player has one Avalon identity. That identity can have completely different,
unrelated characters in different games — Avalon never dictates a character model.
Friends, guilds, and achievements are network-level concepts a game opts into, not
things a game is required to expose or trust blindly. A game stays fully sovereign
over its own world, economy, and rules; Avalon only provides the connective
infrastructure between games, not a platform that owns them.

## Why Avalon

- **Identity is separate from characters.** One persistent identity, any number of
  unrelated characters across games — see [ADR: Identity Is Separate From Game Characters](https://github.com/LunarVagabond/avalon-protocol/issues/67).
- **Interoperability is opt-in.** A game chooses which Avalon capabilities it wants
  (identity, guilds, achievements, ...) and which other issuers' attestations it
  trusts. Nothing is forced.
- **Least privilege by default.** A game receives only the capabilities a player
  has explicitly granted — never blanket access to an identity's whole history.
- **History is verifiable, not just shared.** Achievements are signed attestations
  a receiving game can independently verify and decide whether to trust — not
  arbitrary rows in someone else's database.
- **Settlement is a public transparency log, not federation or blockchain consensus.** Milestone 1 is a signed, append-only ledger. Long-term, durable facts are independently verifiable and mirrorable by anyone — not gated behind servers whitelisting each other, and not requiring mining/consensus to referee a scarcity problem Avalon doesn't have — see [ADR: Attestations Before Blockchain](https://github.com/LunarVagabond/avalon-protocol/issues/68) and [ADR: Settlement Is a Public Transparency Log](https://github.com/LunarVagabond/avalon-protocol/issues/70).
- **Don't build the universe.** Avalon is the railroad between games, not another
  platform trying to own every destination.

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
  stakeholders/
    Proposal.md  the living design document (narrative)
  WhyAvalon.md   the case for why this needs to exist
  architecture/  the normative architecture reference, one file per topic
  players/       docs for people playing games that use Avalon
  developers/    docs for game developers integrating the SDKs
  maintainers/   docs for contributors to this repo
  stakeholders/  docs for people evaluating Avalon from the outside
```

Architecture decisions are tracked as closed GitHub issues labeled
`architecture-decision-record`, not as files in this repo — see
[decided](https://github.com/LunarVagabond/avalon-protocol/issues?q=is%3Aissue+label%3Aarchitecture-decision-record)
and [still-open](https://github.com/LunarVagabond/avalon-protocol/issues?q=is%3Aissue+label%3Adecision+is%3Aopen)
decisions.

## Learn more

| Design | Decisions | Process |
|---|---|---|
| [Proposal](docs/stakeholders/Proposal.md) · [Architecture](docs/architecture/README.md) · [Why Avalon](docs/WhyAvalon.md) | [Decided](https://github.com/LunarVagabond/avalon-protocol/issues?q=is%3Aissue+label%3Aarchitecture-decision-record) · [Open](https://github.com/LunarVagabond/avalon-protocol/issues?q=is%3Aissue+label%3Adecision+is%3Aopen) | [Contributing](.github/CONTRIBUTING.md) |

## Contributing

This repo is private and pre-release; see [CONTRIBUTING.md](.github/CONTRIBUTING.md) for the
workflow once it opens up.

## License

Apache-2.0 — see [LICENSE](LICENSE).
