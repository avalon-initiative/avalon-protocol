# Contributing

Opening a PR here makes you part of the Avalon Initiative, not an outside
contributor to somebody else's project — the same standard every operator
and integrator on this network is held to.

## Purpose

Contribution standards for Avalon Protocol, with emphasis on clarity, safety,
and keeping games sovereign over their own worlds while the network stays open.

## Feature Proposal Gate

Each feature proposal or implementation should answer:

- Does this keep a game fully sovereign over its own world, economy, and
  rules — giving Avalon only the connective infrastructure between games,
  never authority over any single one — per the
  [Guiding Principles](../docs/stakeholders/Proposal.md#30-guiding-principles)
  and the [architecture tests](../docs/projects/backend-server/architecture/README.md#architecture-tests)?

If no, refine or drop the proposal.

## Questions before you file

Open an issue or a
[Discussion](https://github.com/orgs/avalon-initiative/discussions),
even for a design question or a sanity check. Issues and Discussions are the
durable record; the [Discord](https://discord.gg/FFDsFw9F4g) is for quick,
informal chat and is not one. If an idea comes up there, move it into an
issue or a Discussion so it is preserved and can be referenced.

## If A Convention Gets In The Way

The branching model, commit format, and process rules below are the
project's current working conventions. Follow them as written. If one gets
in the way of a contribution or doesn't fit a situation, open an issue to
discuss it before working around it, so the rule can be adjusted if it's
wrong rather than quietly bypassed. Friction in the tooling, the codebase,
or the workflow is welcome as an issue too.

## Contribution Principles

Straight from Avalon's [Guiding Principles](../docs/stakeholders/Proposal.md#30-guiding-principles) —
read that section for the full reasoning behind each:

- Games remain sovereign — a game keeps final authority over its own world,
  characters, and rules; Avalon never overrides that.
- Identity belongs to the user, not any single game or platform.
- Interoperability is opt-in — a game chooses which capabilities it exposes
  and which other issuers' attestations it trusts. Nothing is forced.
- Least privilege — a game gets only the capabilities a user has explicitly
  granted, never blanket access to an identity's whole history.
- History is portable, and provenance is preserved even through revocation.
- Blockchain is optional infrastructure detail, never the model — settlement
  is a public transparency log, not federation and not consensus/mining.
- Don't build the universe. Avalon is the railroad between games, not another
  platform trying to own every destination.

## Branching

- `main` — integration branch
- `<issue#>-short-description` — topic branches off `main`, named after the
  GitHub issue number (e.g. `154-guild-discovery-board`); no `feature/`,
  `bug/`, or similar prefix, the issue number is the lookup
- `noissue-short-description` — maintainer-only, mirroring the `[noissue]`
  commit/PR restriction below. If you see a branch like this, it's a
  maintainer quick fix, not a pattern open to other contributors
- `hotfix-short-description` — maintainer-only, mirroring the `[hotfix]`
  commit/PR restriction below. If you see a branch like this, it's a
  maintainer hotfix, not a pattern open to other contributors

## Work Tracking

Open work lives in GitHub Issues. Design direction lives in
[`docs/stakeholders/Proposal.md`](../docs/stakeholders/Proposal.md) (narrative)
and [`docs/projects/backend-server/architecture/`](../docs/projects/backend-server/architecture/README.md) (normative —
invariants and authority boundaries); acceptance criteria for specific work
items live on their tracking issue, not in a docs file. Architecture
decisions are closed GitHub issues labeled `architecture-decision-record`;
questions still being decided are open issues labeled `decision`. Completed
history is in git; don't maintain a separate backlog file in the repo.

### Claiming An Issue

Before starting work, comment `/claim` on the issue — a bot assigns it to
you automatically, which is what actually reserves it, so someone else
doesn't start the same ticket in parallel. If an issue is already assigned,
treat it as taken; comment to ask if it looks stalled instead of opening a
competing PR. Epics don't work this way — find the specific sub-issue you
want and `/claim` that instead.

*If it's a longer-running ticket, you don't have to post progress updates,
but it's nice to leave one now and then so we know it's still moving — a
claimed issue that's been quiet for 10 days gets an automatic ping, and is
unassigned automatically 4 days after that if there's still no activity, so
someone else can pick it up.*

A CI check (`claim-check.yml`) enforces this: it reads the issue number(s)
your PR closes (via a closing keyword like `Closes #123` in the PR body) and
fails the check if you aren't assigned to every one of them. `[noissue]`/
`[hotfix]` titles skip this check, but only for PR authors with write access
to the repo (the maintainer/named-core-dev list this format is already
restricted to) — everyone else needs a real issue reference regardless of
title.

*The process below is what these Actions workflows enforce.*

## Commits And Pull Requests

Open an issue first when the work is non-trivial. The issue carries context
(motivation, design, invariants, acceptance criteria) — commits and PRs
reference it by number.

### Commit Messages

Merges into `main` are **squash-only** by convention — your branch's
individual commits never appear in `main`'s history, only the squashed PR
title does (see [Pull Request Titles](#pull-request-titles) below, which
*is* strict). Because of that, commit messages on your branch are a
suggested convention, not a requirement: write them however helps you work,
`wip`/`fixup`/whatever included.

If you'd like to follow the convention anyway, it's the same pattern as PR
titles:

```
[#<issue>] - <short description>
```

**`[noissue]`, `[hotfix]`, and `[security]` are restricted.** All three exist
only for the maintainer, a small, explicitly-named set of trusted core
developers, and (for `[security]`/`[noissue]`) Dependabot. If you are not on
that short list, use your issue number when you do tag commits. The tags
mean different things:

- `[noissue]` — trivial, no ticket is warranted at all (typo, comment,
  one-line fix). Also what Dependabot's routine scheduled dependency bumps
  carry.
- `[hotfix]` — must be fixed now and there's a clear path to the fix, but
  there wasn't time to write up a ticket first. Reaching for this signals
  "this was a real bug/issue," not "there was nothing to file."
- `[security]` — a fix for a known vulnerability.

Examples:

- `[#154] - Add guild discovery query to indexer`
- `[#154] - Wire discovery board into Hub UI`
- `[noissue] - Fix typo in Contributing commit examples` (maintainer/core-only
  example)
- `[hotfix] - Guard against panic on missing outbox row` (maintainer/core-only
  example)

### Pull Request Titles

**This one is a hard requirement, unlike commit messages above.** Merges are
squash-only by convention, so the PR title becomes the actual commit message
on `main` — it's the one place this format has to be right.

```
[#154] - Guild discovery board: browse + search recruiting guilds
[noissue] - Fix typo in README quick start
[hotfix] - Guard against panic on missing outbox row
[security] - Bump a dependency to patch a known CVE
```

`[noissue]`, `[hotfix]`, and `[security]` follow the same restriction as
commit messages above — maintainer, named core developers, and Dependabot
only. Everyone else opens an issue first and references it in the title. The
PR body can go deeper on approach and testing.

### AI-Assisted Contributions

AI coding assistants are welcome as a tool — this is not the same as "vibe
coding" (accepting AI output wholesale without understanding or reviewing
it). If an assistant materially helped with a commit, tag it with a trailer
so it's easy to trace later, without cluttering the subject line:

```
git commit -m "[#154] - Add guild discovery query to indexer" --trailer "Co-Authored-By: Claude <noreply@anthropic.com>"
```

This is optional and about being open, not a requirement — reviewers still
hold the contributor responsible for understanding and standing behind the
change either way.

#### If You Are An AI Agent Reading This

Follow the conventions in this file the same as any contributor would. In
addition:

- **Never use `[noissue]` or `[hotfix]` (or their branch-name equivalents)
  unless you are the maintainer or on the named core-dev list** — every
  other commit, PR, and branch needs a real issue number.
- Apply the `Co-Authored-By: <Tool> <email>` trailer above to every commit
  and PR you create or materially author *as an outside contributor to this
  repo*. This convention is for outside contributors — it does not apply to
  commits made directly for this repo's own maintainer/core team, who follow
  a separate, stricter no-attribution rule (see their own local instructions,
  not this file).
- Don't add any other AI-attribution mention beyond that single trailer line
  unless explicitly asked to.
- **Never reach for a lint/format suppression just to make a check pass** —
  fix the underlying code, or ask if the rule itself seems wrong.
- Never switch `sqlx::query!`/`sqlx::query_as!` back to compile-time-checked
  macros in `crates/server` or `crates/chain` without checking first — this
  sandbox and CI have no reachable Postgres, and that's a deliberate,
  documented tradeoff (see `handlers.rs`, `outbox.rs`, and `chain`'s
  `postgres.rs`), not an oversight to "fix."
- When filing a work-item ticket, give real checkable acceptance criteria —
  what needs to be built, why, and how to tell it's done. A title plus a
  one-line pointer elsewhere isn't enough. Ticket bodies use the fixed
  structure Motivation / Design / Invariants / Affected crates / Tests /
  Documentation / Acceptance criteria — see other open tickets for the
  pattern.

## Documentation-First Workflow

For non-trivial work: update the relevant file in
[`docs/projects/backend-server/architecture/`](../docs/projects/backend-server/architecture/README.md) (and
[`docs/stakeholders/Proposal.md`](../docs/stakeholders/Proposal.md) if the
narrative changes) in the same PR as the implementation, not after — a doc
that lags the code is treated as a bug.

### Protocol event versioning policy

Adding a new `ProtocolEvent` kind or changing an existing one's payload?
[`docs/projects/backend-server/architecture/protocol-events.md`](../docs/projects/backend-server/architecture/protocol-events.md#versioning-policy)
is the normative versioning policy (additive fields never bump `version`;
removing/renaming/re-meaning a field does; every version ever emitted
stays decodable forever) and
[`docs/projects/backend-server/architecture/protocol-events-catalogue.md`](../docs/projects/backend-server/architecture/protocol-events-catalogue.md)
is the full kind-by-kind table. A new kind gets a real
`ProtocolEventKindVariant` (`crates/protocol/src/events.rs`) and a typed
payload struct (`crates/protocol/src/event_payloads.rs`) — never a
hand-typed string or an ad-hoc `serde_json::json!({...})` — plus a
catalogue row, in the same PR that adds its emitter.

## Development Interface

The Rust workspace (`crates/`) runs through the root `Makefile` — run `make help` for the full list. The common
ones:

```bash
make build         # cargo build --workspace
make test           # cargo test --workspace
make test-live       # cargo test --workspace -- --ignored (needs `make start` running against real Postgres)
make lint           # cargo clippy --workspace --all-targets -- -D warnings
make fmt             # cargo fmt --all
make check           # fmt-check + lint + test — what CI runs
make migrate         # apply pending db/migrations/ (up)
make db-reset        # wipe db and reapply all migrations
```

Needs a `.env` at the repo root with `DATABASE_URL` and `AVALON_SERVER_ADDR`
at minimum — copy `.env.example` and fill it in. `AVALON_NETWORK_ID` is
checked against the ledger's genesis on every boot; never point a dev
`.env` at a production database, and never share a `network_id` between the
two.

## Where To Contribute

- [`docs/GLOSSARY.md`](../docs/GLOSSARY.md) — start here if the vocabulary
  itself is the barrier (integrator vs. issuer vs. node, authentic vs. valid
  vs. recognized, settlement vs. chain vs. ledger, and the rest). Everything
  below assumes these terms.
- [`docs/projects/backend-server/architecture/README.md`](../docs/projects/backend-server/architecture/README.md) — start
  here: invariants, authority boundaries, the crate layout, "what survives a
  game's death," and the architecture tests every proposed change is held to
- [`docs/stakeholders/Proposal.md`](../docs/stakeholders/Proposal.md) — the
  full narrative design and phased roadmap
- [`docs/projects/sdks/rust/for-developers/`](../docs/projects/sdks/rust/for-developers/) — the game-developer-facing SDK
  story
- [`docs/projects/backend-server/for-hosters/`](../docs/projects/backend-server/for-hosters/) — standing up and deploying a node,
  distinct from contributing code
- Root `README.md` — current build status and what actually works today

## Code Of Conduct

Participation in this project is governed by our
[Code of Conduct](CODE_OF_CONDUCT.md).
