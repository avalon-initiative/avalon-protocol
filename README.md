# Avalon Protocol

Avalon Protocol is an open, Rust-based interoperability layer for independent games:
one persistent player identity, one social graph, that a player carries between
games that would otherwise treat every login as a stranger.

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
owned by the player, not any single game.

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

## Where Discord fits

Discord already gets part of this right: one identity, one friends list,
presence and communities that already span every game you play — exactly the
problem this README opens with. What it doesn't give you is anything a game
can actually build on: there's no achievement a game can issue and another
can independently verify, no way for a game to trust a claim made outside its
own database, and your social graph doesn't belong to you in any portable
sense — it belongs to Discord's platform, not you. Discord is a real, working
middle ground for presence and social continuity, not a competitor to Avalon.
There's no reason Discord couldn't become a *client* of Avalon Protocol —
surfacing a player's real identity, friends, and verifiable achievements
inside a server people already live in — the same way a game or the Hub app
are clients today.

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
