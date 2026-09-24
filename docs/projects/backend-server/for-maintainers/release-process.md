# Cutting a release

How a maintainer ships a new version of `avalon-server` (and the protocol
crates it's built from) — version numbering, what `make release` does, and
how CI turns a pushed tag into a published GitHub Release with binaries. For
what a hoster does with a release once it exists, see
[`upgrading.md`](../for-hosters/upgrading.md).

**This is the designed process, not fully live yet** — `make release` and
the release CI workflow described below are being built to match this doc
rather than the other way around. This repo is also private and its GitHub
Actions workflows stay disabled at the repo-settings level until it goes
public (see `.claude/CLAUDE.md`'s conventions), so even once the workflow
file exists, it won't actually fire until that flips. Treat this page as
the spec both the tooling and this doc's own future edits are held to.

## Scope

This process versions and releases `avalon-server` and the protocol crates
in this workspace (`crates/protocol`, `crates/chain`, `crates/indexer`,
`crates/server`, `crates/cli`, `crates/devenv`) as one unit, under one
shared version — they already share a single `version.workspace = true` in
the root `Cargo.toml`'s `[workspace.package]`, so there's no meaningful way
for them to drift independently. It does **not** cover:

- The Rust reference SDK, which lives in the separate `avalon-sdks` repo and
  releases on its own schedule.
- The C# and TypeScript SDKs (`avalon-sdks`) and the Hub/hub-app apps
  (`avalon-hub`) — each lives in its own repository with its own version and
  release process, independent of the server's version.

## Versioning: SemVer, and what each octet actually means here

`X.Y.Z`, standard [SemVer](https://semver.org/) format, but read the three
octets against **integrator impact** specifically, not the generic SemVer
definitions:

- **`Z` — non-breaking upgrade.** Bug fixes, security patches, performance
  work, anything where an integrator (or a hoster) does nothing differently
  and everything keeps working exactly as before. This is where the
  overwhelming majority of releases should land — **keeping changes at `Z`
  as much as possible is a deliberate goal of this project**, not just what
  happens to be true most of the time.
- **`Y` — internal change, still non-breaking for integrators.** Something
  meaningful changed on the inside — new capability, new migration,
  internal restructuring, a new optional feature — and behavior may look
  different in places, but no existing integrator's working integration
  breaks without them changing anything. Wire format, SDK surface, and
  signing bytes are all still compatible with the previous `Y`.
- **`X` — breaking. Integrators have to update.** The wire format changed,
  an API route or SDK surface was removed or changed incompatibly, or the
  signing-byte format changed (the specific thing
  [`crates/protocol/tests/conformance.rs`](../../../../crates/protocol/tests/conformance.rs)
  and `avalon-sdks`' own `rust/tests/conformance.rs` both exist to catch —
  see epic #771/#774 in the architecture notes). A real `X` bump should be
  rare and deliberate, announced ahead of the tag going out where
  possible, not just discovered by an integrator's client breaking after
  an upgrade — see #797 for what "breaking" should actually look like at
  request time (a clean, typed rejection, never a silent misinterpretation
  that risks dropped or corrupted data) once that's decided.

When in doubt whether something is `Y` or `X`: if an existing, unmodified
integrator client (an existing SDK version, talking to the new server)
keeps working exactly as it did, it's `Y`. If it doesn't, it's `X` — no
exceptions for "it's a small change."

## Release cadence

Releases should land on a predictable, reasonable schedule, not purely
whenever something happens to be ready. The point isn't ceremony — it's
that an integrator or hoster should never feel like they need to
constantly watch the Releases page to avoid falling behind; a predictable
cadence (mostly `Z`-level, as above) plus the update-availability signal
tracked by issue #795 are meant to make "am I on the latest version"
something that surfaces to a hoster on its own, not something that has to
be manually checked. No fixed interval is committed to yet — set one once
there's enough real release history to know what's sustainable, rather
than guessing here.

## A release doesn't reach the network all at once

Publishing a release doesn't push it to every running node — nothing can,
by design (no party can force an operator to upgrade; see
[`../architecture/nodes.md`](../architecture/nodes.md)). In practice,
shipping an upgrade to even a handful of well-connected nodes starts a
real, organic upgrade chain rather than requiring every hoster to be
watching the Releases page directly:

- Internally, deploy new releases to the network's few most-connected
  seed/mirror nodes first — the nodes other operators commonly mirror.
- Each of those nodes' own direct peers picks up the new
  `protocol_version` automatically via the existing peer-gossip signal
  (#368 — `GET /nodes/status`'s `stale`/`newest_known_peer_version`,
  derived straight from each node's own peer table). No message gets
  relayed beyond direct peers, and nothing is auto-applied — see #797 for
  exactly why unsigned relay/rebroadcast isn't safe to build.
- Those operators see the signal, upgrade on their own schedule, and their
  own peers pick it up in turn — awareness spreads hop-by-hop through the
  real peer graph over time, faster the better-connected the network is.
- See [`upgrading.md`](../for-hosters/upgrading.md#rolling-an-upgrade-out-across-a-mirroredmulti-node-network)
  for the hoster-facing side of this same mechanism.

This is genuinely slower than a central push, and that's an accepted
tradeoff, not an oversight — see #619 for what a faster, still-safe
mechanism (opt-in auto-update against a *signed* release manifest, gated
on a release-signing key so a bad actor can't forge "an update exists")
would need before it's safe to build.

## Steps to cut a release

### 1. Decide the version

Pick `X.Y.Z` using the policy above, based on what's actually landed on
`main` since the last release — not a guess made in advance. `git log
<last-tag>..main --oneline` is the fastest way to see what's actually
shipping.

### 2. Run `make release`

From `main`, with a clean working tree:

```bash
make release
# or non-interactively:
make release VER=0.4.0
```

If `VER` isn't passed, `make release` shows the current version and prompts
for the new one. Before touching any files, it runs the full pre-release
gate — `make lint`, `make build`, `make test`, and `make test-live` (against
a running `make start` stack) — and **aborts with nothing changed** if any
of it fails. There's no partial-release state to clean up after a failed
attempt; fix the failure and re-run.

Once the gate passes, `make release`:

1. Bumps `version` under `[workspace.package]` in the root `Cargo.toml` (and
   regenerates `Cargo.lock` for the version change) — every workspace crate
   picks this up automatically via `version.workspace = true`.
2. Commits `Release vX.Y.Z` (only if the version bump actually changed
   anything — safe to re-run against a version that's already current).
3. Creates an annotated tag `vX.Y.Z`.

### 3. Push the tag

```bash
git push origin main --tags
```

Pushing a `v*` tag is what triggers the release build — an ordinary push to
`main` without a tag does not build or publish anything.

### 4. Let CI build

The release workflow runs only on a pushed `v*` tag: builds `avalon-server`,
its `migrate` companion binary, and the `avalon` CLI binary; packages them
(a `.tar.gz` per target plus the Docker image this repo's `Dockerfile`
already produces for `make stack-up`); and opens a **draft** GitHub Release
on the tag with those binaries attached and an auto-generated changelog
from the commits since the last tag.

### 5. Review and publish the draft

Open the draft on the repo's [Releases page](https://github.com/avalon-initiative/avalon-protocol/releases),
check the attached binaries and changelog, edit the notes if needed
(the `X`/`Y`/`Z` meaning above is worth restating plainly for anyone
skimming release notes to decide whether to upgrade immediately), and
publish it. Until published, hosters following
[`upgrading.md`](../for-hosters/upgrading.md) won't see it as the latest
release.

## Hotfixes

A `Z` security patch follows the exact same steps above — there's no
separate hotfix process. If the fix needs to ship before whatever else is
sitting on `main` is ready, cut it from a dedicated branch off the last
release tag instead of `main`, and cherry-pick just the fix onto it before
running `make release` from there.
