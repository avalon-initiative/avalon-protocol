# Documentation

This tree is split by audience, not by topic — pick the one that's you, or
start with the glossary if the vocabulary itself is the barrier.

| I am... | Start here |
|---|---|
| New to the project, or the terminology is the blocker | [`GLOSSARY.md`](GLOSSARY.md) |
| Evaluating Avalon from the outside (backing it, partnering, deciding whether to build on it) | [`stakeholders/`](stakeholders/README.md) |
| A player/user of a game, app, or service that integrates Avalon | [`users/`](users/README.md) |
| Integrating the SDK into my own game, app, or service | [`developers/`](developers/README.md) |
| Contributing code or docs to this repository | [`maintainers/`](maintainers/README.md) |
| Standing up an `avalon-server` node | [`hosters/`](hosters/README.md) |
| Reading or reviewing the normative design (invariants, authority boundaries, what exists today) | [`architecture/`](architecture/README.md) |

## Suggested reading order for a first-time contributor

1. Root [`README.md`](../README.md) — what Avalon is, in plain language.
2. [`GLOSSARY.md`](GLOSSARY.md) — the vocabulary used everywhere below. Most
   of what makes the architecture docs hard to skim on a first read is
   unfamiliar words used precisely (integrator vs. issuer vs. node,
   authentic vs. valid vs. recognized, settlement vs. chain vs. ledger) —
   this page exists so you're not decoding that *and* the argument at the
   same time.
3. [`architecture/README.md`](architecture/README.md) — the map: the full
   document index, the invariant table, and "what survives an integrator's
   death," which is the fastest way to feel the shape of the whole system
   in one sitting.
4. Whichever [`architecture/`](architecture/) topic doc matches the area
   you're about to touch. One file per topic; each ends with a "Today in
   the repo" section pointing at the actual code, more current than any
   prose summary elsewhere will stay.
5. [`maintainers/README.md`](maintainers/README.md) →
   [`../.github/CONTRIBUTING.md`](../.github/CONTRIBUTING.md) for the
   actual contribution workflow (branching, commit/PR format, issue
   claiming).

## How this tree is organized

- **Audience directories** (`users/`, `developers/`, `maintainers/`,
  `hosters/`, `stakeholders/`) are task-oriented: what do you need to *do*.
- [`architecture/`](architecture/) is the one normative reference,
  audience-agnostic: what's *true* about the system and why, one file per
  topic, each linking the GitHub issue that decided it.
  [`stakeholders/Proposal.md`](stakeholders/Proposal.md) is the narrative
  companion — same ideas, prose instead of invariants — and defers to
  `architecture/` whenever the two disagree.
- [`GLOSSARY.md`](GLOSSARY.md) is the fast lookup that ties the two
  together.
- [`WhyAvalon.md`](WhyAvalon.md) is the case for why this needs to exist at
  all, referenced by more than one audience directory above.

Architecture *decisions* are tracked as closed GitHub issues labeled
[`architecture-decision-record`](https://github.com/LunarVagabond/avalon-protocol/issues?q=is%3Aissue+label%3Aarchitecture-decision-record),
not as files anywhere in this tree — see
[`architecture/README.md`](architecture/README.md) for why and where
still-open questions (label `decision`) are tracked instead.
