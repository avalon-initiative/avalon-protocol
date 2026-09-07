# Contributing

<!--
Fill in every <<PLACEHOLDER>> below from what you can infer about this repo
(README, existing docs, git log) and ask the user only for what you can't.
Delete any section whose feature isn't installed on this repo (e.g. the
Claiming An Issue section only makes sense if claim-issue.yml/claim-check.yml
are installed, which only happens for OSS/public repos).
-->

## Purpose

Contribution standards for <<project name>>, with emphasis on clarity and
safety.

<!-- Delete this section if the project has no scope filter / design
principles doc to check proposals against. -->
## Feature Proposal Gate

Each feature proposal or implementation should answer:

- <<one-line restatement of the project's core scope filter — e.g. "does
  this keep the core small and push project-specific behavior to
  configuration/plugins?">>

If no, refine or drop the proposal.

## Questions before you file

<<If Discussions is enabled: point contributors there for a sanity check
before opening an issue. Otherwise drop this section.>>

## If A Convention Gets In The Way

The branching model, commit format, and process rules below are a starting
point, not a settled standard. Follow them as written. But if one is
genuinely getting in the way of a contribution, doesn't fit a situation, or
just seems off, raise it first — an issue or Discussion — before working
around it. Same goes for friction in the tools, the codebase, or the workflow
generally: surfacing it is always welcome.

## Branching

- `main` — integration branch
- `<issue#>-short-description` — topic branches off `main`, named after the
  GitHub issue number; no `feature/`/`bug/` prefix, the issue number is the
  lookup
<!-- Delete the two lines below if this repo doesn't restrict any commit/PR
tags to a maintainer allowlist. -->
- `noissue-short-description` — maintainer-only
- `hotfix-short-description` — maintainer-only

## Work Tracking

Open work lives in GitHub Issues. Completed history is in git; don't
maintain a separate backlog file in the repo.

<!-- Delete this whole "Claiming An Issue" section if claim-issue.yml /
claim-check.yml aren't installed on this repo (private/non-OSS repos skip
these workflows by default). -->
### Claiming An Issue

Before starting work, comment `/claim` on the issue — a bot assigns it to
you automatically, which is what actually reserves it. If an issue is
already assigned, treat it as taken; comment to ask if it looks stalled
instead of opening a competing PR. Epics don't work this way — find the
specific sub-issue you want and `/claim` that instead.

*If it's a longer-running ticket, you don't have to post progress updates,
but it's nice to leave one now and then — a claimed issue that's been quiet
for 10 days gets an automatic ping, and is unassigned automatically 4 days
after that if there's still no activity.*

A CI check (`claim-check.yml`) enforces this: it reads the issue number(s)
your PR closes (via a closing keyword like `Closes #123`) and fails if you
aren't assigned to every one of them. `[noissue]`/`[hotfix]` titles skip this
check, but only for PR authors with write access to the repo.

## Commits And Pull Requests

Open an issue first when the work is non-trivial. The issue carries context —
commits and PRs reference it by number.

### Commit Messages

<<If merges are squash-only, say so here — individual commit messages on the
branch are then just a convention, not a requirement, since only the PR title
survives onto main.>>

If you'd like to follow the convention anyway, it's the same pattern as PR
titles below.

<!-- Delete this restricted-tags block if this repo has no maintainer-only
tag convention. -->
**`[noissue]`, `[hotfix]`, and `[security]` are restricted.** All three exist
only for the maintainer, a small explicitly-named set of trusted core
developers, and (for `[security]`/`[noissue]`) Dependabot.

- `[noissue]` — trivial, no ticket is warranted at all.
- `[hotfix]` — must be fixed now and there's a clear path to the fix, but
  there wasn't time to write up a ticket first.
- `[security]` — a fix for a known vulnerability.

### Pull Request Titles

**Title format:** `<<e.g. "[#<issue>] - <short description>", or
"type(scope): summary", or whatever this repo actually uses>>`

<<If merges are squash-only, note that the PR title becomes the actual commit
message on main — it's the one place this format has to be right.>>

Examples:

```
<<[#123] - Add real example title in this repo's actual format>>
```

### AI-Assisted Contributions

AI coding assistants are welcome as a tool — this is not the same as "vibe
coding" (accepting AI output wholesale without understanding or reviewing
it). If an assistant materially helped with a commit, tag it with a trailer
so it's easy to trace later, without cluttering the subject line:

```
git commit -m "<<title>>" --trailer "Co-Authored-By: Claude <noreply@anthropic.com>"
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
- **Never reach for a lint/format suppression just to make a check pass** — fix
  the underlying code, or ask if the rule itself seems wrong.
- When filing a work-item ticket, give real checkable acceptance criteria —
  what needs to be built, why, and how to tell it's done. A title plus a
  one-line pointer elsewhere isn't enough.

## Documentation-First Workflow

<<Delete if this repo has no docs/ convention.>> For major work: update the
relevant file in `docs/` first, align implementation with the accepted docs,
then update docs and behavior together on later changes.

## Development Interface

<<The canonical local dev entry point — Makefile/package.json scripts/
justfile/etc. Include the common commands (install, build/dev, test, lint)
and how to point tests at any required local infra (env vars, .env.example).>>

## Where To Contribute

<<Links to this repo's actual docs entry points: getting-started doc,
architecture doc, specs, roadmap/decisions if tracked as labeled issues.>>

## Code Of Conduct

Participation in this project is governed by our
[Code of Conduct](CODE_OF_CONDUCT.md).
