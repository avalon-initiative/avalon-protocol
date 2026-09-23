# Documentation

This tree is organized two ways at once, and both are meant to get you to the
same content:

- **By audience** — pick who you are in the table below.
- **By project** — [`projects/`](projects/README.md) has one folder per
  deployable thing in this repository (the backend, each SDK, the Hub, ...),
  each self-contained enough to move to its own repo later. If you already
  know which part of Avalon you care about, start there instead.

Start with [`GLOSSARY.md`](GLOSSARY.md) if the vocabulary itself is the
barrier — most of what makes any of this hard to skim on a first read is
unfamiliar words used precisely (integrator vs. issuer vs. node, authentic
vs. valid vs. recognized, settlement vs. chain vs. ledger), not the ideas
themselves.

## By audience

| I am... | Start here |
|---|---|
| New to the project, or the terminology is the blocker | [`GLOSSARY.md`](GLOSSARY.md) |
| Evaluating Avalon from the outside (backing it, partnering, deciding whether to build on it) | [`stakeholders/`](stakeholders/README.md) |
| A user of a game, app, or service that integrates Avalon | [`users/`](users/README.md) |
| Integrating an SDK into my own game, app, or service | [`projects/sdks/`](projects/sdks/README.md) |
| Contributing code or docs to this repository | [`maintainers/`](maintainers/README.md) |
| Standing up an `avalon-server` node | [`projects/backend-server/for-hosters/`](projects/backend-server/for-hosters/README.md) |
| Reading or reviewing the normative design (invariants, authority boundaries, what exists today) | [`projects/backend-server/architecture/`](projects/backend-server/architecture/README.md) |

## By project

| Project | What it is |
|---|---|
| [`projects/backend-server/`](projects/backend-server/README.md) | The network itself — identity, social graph, guilds, achievements, settlement. What everything else talks to. |
| [`projects/sdks/`](projects/sdks/README.md) | Every official SDK (Rust, C#, TypeScript), one per language, one language-agnostic design reference. |
| [`projects/cli/`](projects/cli/README.md) | `avalon`, the local dev/ops CLI. |
| [`projects/hub/`](projects/hub/README.md) | The web client — a user's first doorway into Avalon. |
| [`projects/mobile-hub/`](projects/mobile-hub/README.md) | The Tauri desktop/mobile shell around the Hub UI. |
| [`projects/ui/`](projects/ui/README.md) | The shared Vue3 component library both Hub apps use. |

See [`projects/README.md`](projects/README.md) for why the split is drawn
where it is, and what stays cross-cutting instead of moving into one project
folder.

## Suggested reading order for a first-time contributor

1. Root [`README.md`](../README.md) — what Avalon is, in plain language.
2. [`GLOSSARY.md`](GLOSSARY.md) — the vocabulary used everywhere below.
3. [`projects/backend-server/architecture/README.md`](projects/backend-server/architecture/README.md) —
   the map: the full document index, the invariant table, and "what survives
   an integrator's death," which is the fastest way to feel the shape of the
   whole system in one sitting.
4. Whichever [`projects/backend-server/architecture/`](projects/backend-server/architecture/)
   topic doc matches the area you're about to touch. One file per topic,
   describing the system as it stands today.
5. [`maintainers/README.md`](maintainers/README.md) →
   [`../.github/CONTRIBUTING.md`](../.github/CONTRIBUTING.md) for the
   actual contribution workflow (branching, commit/PR format, issue
   claiming).

## How this tree is organized

- **Audience directories** at the top level (`users/`, `maintainers/`,
  `stakeholders/`) cover things that span more than one project, or haven't
  been split into a project folder yet. Where a doc is specific to one
  deployable, it lives under that project's own `for-<audience>/` folder
  instead — e.g.
  [`projects/backend-server/for-hosters/`](projects/backend-server/for-hosters/README.md),
  [`projects/sdks/rust/for-developers/`](projects/sdks/rust/for-developers/README.md) —
  see [`projects/README.md`](projects/README.md).
- [`projects/backend-server/architecture/`](projects/backend-server/architecture/README.md)
  is the one normative reference for the protocol itself, audience-agnostic:
  what's *true* about the system and why, one file per topic, each linking
  the GitHub issue that decided it.
  [`stakeholders/Proposal.md`](stakeholders/Proposal.md) is the narrative
  companion — same ideas, prose instead of invariants — and defers to
  `architecture/` whenever the two disagree.
- [`GLOSSARY.md`](GLOSSARY.md) is the fast lookup that ties audience and
  project docs together.
- [`WhyAvalon.md`](WhyAvalon.md) is the case for why this needs to exist at
  all, referenced by more than one audience directory above.

Architecture *decisions* are tracked as closed GitHub issues labeled
[`architecture-decision-record`](https://github.com/LunarVagabond/avalon-protocol/issues?q=is%3Aissue+label%3Aarchitecture-decision-record),
not as files anywhere in this tree — see
[`projects/backend-server/architecture/README.md`](projects/backend-server/architecture/README.md)
for why and where still-open questions (label `decision`) are tracked
instead.
