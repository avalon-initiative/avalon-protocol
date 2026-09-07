# Avalon Protocol

**Status:** scaffolding stage. The Rust workspace (`protocol`, `chain`, `indexer`,
`server`, `sdk`, `cli`), the web/mobile Hub apps, and the C# SDK skeleton all exist
and build, but no real server, database, or auth logic is wired up yet. See
[`docs/Proposal.md`](docs/Proposal.md) for the full design and
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
- **Blockchain is optional, not foundational.** Milestone 1 is a signed,
  append-only ledger; whether and how an actual chain fits in later is a real open
  decision, not assumed — see [ADR: Attestations Before Blockchain](https://github.com/LunarVagabond/avalon-protocol/issues/68).
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
  Proposal.md    the living design document
  WhyAvalon.md   the case for why this needs to exist
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
| [Proposal](docs/Proposal.md) · [Why Avalon](docs/WhyAvalon.md) | [Decided](https://github.com/LunarVagabond/avalon-protocol/issues?q=is%3Aissue+label%3Aarchitecture-decision-record) · [Open](https://github.com/LunarVagabond/avalon-protocol/issues?q=is%3Aissue+label%3Adecision+is%3Aopen) | [Contributing](CONTRIBUTING.md) |

## Contributing

This repo is private and pre-release; see [CONTRIBUTING.md](CONTRIBUTING.md) for the
workflow once it opens up.

## License

Apache-2.0 — see [LICENSE](LICENSE).
