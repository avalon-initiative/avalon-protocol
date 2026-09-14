# Self-Hosting a Private Instance

**Running your own instance of the code is fully supported.** Doing so does
not join the Avalon network — it forks it. Those are two different things,
and this document exists so that distinction is never fuzzy for a studio
deciding which one they actually want.

## The two things people mean by "self-host"

1. **Mirroring the public network.** An operator runs `avalon-server`
   (Settlement/Indexer/Gateway, any combination) against the *same*
   `network_id` as the reference deployment, syncing the same public,
   verifiable log. This is what [`./nodes.md`](./nodes.md) means by "multiple
   operators" and what [`../stakeholders/Proposal.md` §20](../stakeholders/Proposal.md#20-self-hosting-and-decentralization)
   describes — more infrastructure serving the one network, the Certificate
   Transparency pattern, not federation.
2. **Running a private, disconnected instance.** An organization runs the
   exact same code, but roots it under its own `network_id` — a studio
   spinning up Avalon for its own internal integrators, an air-gapped environment,
   a staging/dev copy that should never touch production data. This document
   is about this case.

Both are "self-hosting" in casual conversation. Only the first one is part of
the Avalon network. The second is deploying the software, not joining the
protocol's community of integrators and identities — same code, disconnected data.

## Why this is safe to offer, and exactly where the line is

Every ledger entry is hashed with its `network_id` folded in ahead of the
rest of the content ([`./settlement.md`](./settlement.md),
[#173](https://github.com/LunarVagabond/avalon-protocol/issues/173)), and
`network_id` is committed once, at genesis, into a dedicated
`chain_genesis` row — every later boot reads that stored value back and
refuses to start if the configured `AVALON_NETWORK_ID` doesn't match it
exactly (`crates/chain/src/postgres.rs`). Two consequences fall out of that
by construction, not by policy:

- A private instance's history is **cryptographically incapable** of being
  mistaken for, merged into, or replayed against the public network's log —
  the hashes don't collide, by design, the same way two Certificate
  Transparency logs with different tree identities don't collide.
- There is no accidental path from "internal dev deployment" to "silently
  part of the public network" — the mismatch check fails fast and loud,
  before the process even binds a listener.

That's what makes this safe to explicitly support rather than something to
merely tolerate: a private deployment can never leak into or corrupt the
network everyone else is relying on, and there's no migration path that
quietly blurs the two.

## What a private instance gets, and what it gives up

A private instance is the real thing — identity, guilds, achievements,
attestations, the same trust model — running for one organization's own
integrators. What it does **not** get is the reason most of this exists:

- Its identities, guilds, and achievements are meaningless outside itself —
  nothing on the public network can see, verify, or recognize anything a
  private instance issues, and vice versa. [`./trust-model.md`](./trust-model.md)'s
  "authentic, valid, recognized" distinctions all still apply, but recognition
  can only ever happen among integrators pointed at the same `network_id`.
- None of the cross-integrator discovery, shared communities, or "your friends are
  already here" effects in
  [`../developers/WhyBuildOnAvalon.md`](../developers/WhyBuildOnAvalon.md)
  apply — those come from the network, not the code.
- It's a fork in substance, even though it's zero effort in practice (set a
  different `AVALON_NETWORK_ID` and stand up your own Postgres). Nothing
  merges the two later without a real migration; there is no "upgrade path"
  from private to public that isn't just "start recognizing the public
  network's attestations going forward."

## Where this actually makes sense

Legitimate reasons to run a private instance instead of pointing at the
public network:

- Local dev, CI, and staging environments — every `avalon-dev-<name>`
  `network_id` in `.env.example` is already exactly this
- An internal tooling/QA environment that should never touch real identity data
- A studio piloting Avalon internally before committing to the public network
- An organization with real regulatory, contractual, or air-gap constraints
  that make joining any shared network a non-starter

None of these are the goal of the project. Avalon exists to be the shared
identity and social layer between *independent* integrators — the value described
in [`../developers/WhyBuildOnAvalon.md`](../developers/WhyBuildOnAvalon.md)
compounds with the size of the *public* network, not with how many private
forks of the code exist. A studio is always welcome to run their own
instance; we'd just rather have their integrators on the actual network, where
their users' identities and communities are worth something beyond that
one studio's integrators. If a private deployment starts asking "how do we
eventually connect this to the real network" — the honest answer today is
"there isn't one yet beyond re-registering integrators and re-issuing attestations
on the public network" — that's a real gap, not a hidden feature; see
[Decisions and tickets](#decisions-and-tickets) below.

## Today in the repo

- `AVALON_NETWORK_ID` is a required env var
  (`crates/server/src/main.rs`); there is no default, and no way to boot
  without one.
- Genesis commit-and-check lives in
  `PostgresSettlementProvider::connect` (`crates/chain/src/postgres.rs`),
  covered by `connect_fails_fast_on_network_id_mismatch` and
  `connect_succeeds_when_network_id_matches_existing_genesis` in
  `crates/chain/tests/settlement.rs`.
- `network_id` is hashed into every ledger entry
  (`hash_entry`/`hash_event`) and into every Signed Tree Head's signing
  message (`crates/chain/src/sth.rs`), so it's load-bearing for integrity,
  not just a boot-time label.
- `avalon inspect-ledger`/`-full` (`crates/cli`) print whichever
  `network_id` the connected database is actually rooted in, so an operator
  can always confirm which instance they're looking at.
- There is exactly one reference deployment target today
  (`avalon-mainnet-1`, per `.env.example`'s convention) and no tooling yet
  that assumes multiple simultaneous `network_id`s in one process — a
  private instance today means a wholly separate deployment, not a mode flag.
- Whether a private instance or a mirror of the public network, any instance
  reachable beyond localhost must run behind TLS termination — see
  [`../maintainers/deployment.md`](../maintainers/deployment.md).
- Standing up a single instance no longer requires a Rust toolchain —
  `make stack-up` (issue #289, root `Dockerfile` + `docker-compose.yml`)
  builds and runs `avalon-server` + Postgres from a fresh checkout,
  generating a fresh `AVALON_SETTLEMENT_SIGNING_KEY`/`AVALON_NETWORK_ID`
  into `.env` on first run rather than requiring either to be hand-set
  first. See [`../maintainers/hosting-quickstart.md`](../maintainers/hosting-quickstart.md).
- Plain `.env` storage for `AVALON_SETTLEMENT_SIGNING_KEY` is a decided
  floor (issue #352), not an oversight — a dedicated secrets backend was
  weighed and rejected as the default because it would fork `make
  stack-up`'s single zero-manual-steps bring-up path per OS/platform.
  `make stack-up` now `chmod`s the generated `.env` to `600` (#354), and
  [`../maintainers/key-rotation.md`](../maintainers/key-rotation.md)
  documents the rotation procedure that decision left open (#315).
- Structured logging (issue #265): `avalon-server` logs via `tracing`, with
  an HTTP request span (method/path/status/latency) per request via
  `tower-http`'s `TraceLayer`. `RUST_LOG` (standard env-filter syntax)
  controls level without a rebuild; `AVALON_LOG_FORMAT=json` switches from
  the human-readable dev format to one JSON object per line, the shape a
  self-hoster's log aggregator (Grafana/Loki, etc.) expects. See
  [`../maintainers/local-development.md`](../maintainers/local-development.md#logs-and-run-state).

## Decisions and tickets

- [#173](https://github.com/LunarVagabond/avalon-protocol/issues/173)
  `network_id` genesis commitment and mismatch-is-fatal design
- [#70](https://github.com/LunarVagabond/avalon-protocol/issues/70),
  [#79](https://github.com/LunarVagabond/avalon-protocol/issues/79),
  [ADR #186](https://github.com/LunarVagabond/avalon-protocol/issues/186) —
  why mirroring the *same* network is the decentralization story, not
  federation between different ones
- No open ticket yet on whether/how a private instance's history could ever
  be selectively re-issued onto the public network (see the last paragraph
  above) — worth filing as a `decision` issue if a studio actually asks for
  it, rather than speculating here
