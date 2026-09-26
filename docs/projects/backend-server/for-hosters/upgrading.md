# Upgrading and patching a running node

How to move an already-running `avalon-server` node — one that's been
through [`hosting-quickstart.md`](hosting-quickstart.md) and, if reachable
beyond `127.0.0.1`, [`deployment.md`](deployment.md) — onto newer code,
including a security patch that can't wait. This is the "it's already live,
don't break it" path; if you're standing up a node for the first time, use
`hosting-quickstart.md` instead.

Everything below is the manual, step-by-step version. A single
`make upgrade` command that does the backup/upgrade/verify sequence for
you is tracked by issue #796, not built yet — once it lands, it'll be
documented up top here as the fast path, with these steps kept as the
"what it's actually doing" reference.

Releases are tagged and versioned with [SemVer](https://semver.org/)
(`X.Y.Z`) — see [`release-process.md`](../for-maintainers/release-process.md)
for exactly what each octet means and how a release gets cut. Read that
meaning before upgrading, not after:

- **`Z` bump** — safe, non-breaking. Upgrade whenever convenient; this is
  where the overwhelming majority of releases land.
- **`Y` bump** — internal change, still non-breaking for anything already
  connected to this node. Fine to upgrade on your own schedule, but worth
  reading the release notes first.
- **`X` bump** — breaking for integrators. If this node is shared —
  anything other than a single game/app/service you also control — **tell
  the integrators connected to it before upgrading**, since their existing
  client may stop working the moment this node moves to the new version.

**This doc assumes the release process and CI pipeline described in
`release-process.md` exist** — as of this writing that pipeline is designed
but not yet built (`make release` and the release CI workflow are still
being wired up). Every step below is written in terms of a tagged release
on the [Releases page](https://github.com/avalon-initiative/avalon-protocol/releases)
**deliberately, not a commit hash** — developers, nodes, and hosters are
all expected to reason about "which release" the same way, off the same
tag. Until a first tagged release actually exists, there is no supported
way to upgrade a live node via this doc yet; wait for one rather than
substituting an arbitrary commit.

## Before you start: back up

Every upgrade that runs a new migration is a one-way door unless you have a
backup to fall back to. Do this every time, not just for large changes —
a "small" patch can still ship a bad migration.

1. **Back up the database.** From the host running the stack:

   ```bash
   docker exec avalon-postgres pg_dump -U avalon -d avalon -F c -f /tmp/avalon-backup.dump
   docker cp avalon-postgres:/tmp/avalon-backup.dump ./avalon-backup-$(date +%Y%m%d-%H%M%S).dump
   ```

   Move that `.dump` file somewhere off the host itself (it's your only way
   back if the upgrade goes wrong and the host's disk is what fails).

2. **Back up `.env`.** It holds `AVALON_SETTLEMENT_SIGNING_KEY` and every
   other deployment-specific value — losing it is losing the ability to
   extend this node's ledger under its existing history (see
   [`hosting-quickstart.md`](hosting-quickstart.md)'s "Generated `.env`
   values are yours to keep"). Copy it somewhere safe, keeping the same
   `600` permissions.

3. **Note the tag you're currently running**, so you have something
   concrete to roll back to:

   ```bash
   git -C /path/to/avalon-protocol describe --tags --exact-match \
     > ./avalon-previous-release-tag.txt \
     || { echo "not currently on a tagged release — resolve that before upgrading, don't substitute a commit"; exit 1; }
   ```

## Routine upgrade

1. **Pick the release and check it out.** Check the
   [Releases page](https://github.com/avalon-initiative/avalon-protocol/releases)
   for the latest tag and its notes — confirm the `X.Y.Z` bump matches what
   you're prepared for (see the semver callout above), then:

   ```bash
   cd /path/to/avalon-protocol
   git fetch origin --tags
   git checkout v1.2.3   # the tag you're upgrading to
   ```

   Always a tag, never `main` or a bare commit — the whole point of the
   release process is that "which version is this node on" has one
   unambiguous, checkable answer everyone (hosters, integrators, and the
   update-availability signal in #795) can agree on. `main` is a
   development branch, not something a live node should be running.

2. **Check for migrations.** Anything new under
   `crates/server/db/migrations/` since your last upgrade will be applied
   automatically in the next step — read them first if you want to know
   what's about to change in the schema, especially anything destructive
   (dropped columns, `NOT NULL` backfills).

   The full migration set is compiled into both `avalon-server` and the
   `migrate` binary, so a built binary needs no source tree to migrate a
   database: `avalon-server` applies pending migrations on start, and
   `migrate up|down|reset` works from any host that can reach the
   database. Applied migrations are checksummed exactly as before. Set
   `AVALON_MIGRATIONS_DIR` to a directory of `<version>_<name>/{up,down}.sql`
   folders to use that set instead of the embedded one (development only).

3. **Rebuild and restart.** `make stack-up` is safe to re-run against an
   already-running stack — with new code checked out, it rebuilds the
   `avalon-server` and `migrate` images (`--build`), applies whatever
   migrations haven't already been applied, and restarts the
   `avalon-server` container with the new image:

   ```bash
   make stack-up
   ```

   This is a brief-downtime restart, not a rolling/zero-downtime one — a
   single-node deployment has one `avalon-server` container, and it goes
   down for the seconds it takes to swap in the new image. See
   "Near-zero-downtime upgrades" below for a manual blue/green approach if
   even that brief gap matters for your deployment.

4. **Verify.**

   ```bash
   make stack-logs        # confirm avalon-server started cleanly, no panics
   docker compose ps      # confirm both containers report healthy/running
   cargo run -p avalon-cli --bin avalon -- inspect-ledger   # or your equivalent CLI invocation — confirms the ledger is intact and advancing
   ```

   Also do a real end-to-end check appropriate to what changed — log in
   through the Hub, hit an endpoint you know exercises the new code, etc.
   `make stack-logs` will not catch a change that's silently wrong, only
   one that's loudly broken.

5. **If this node has mirrors watching it or watches peers**, confirm the
   mirror-watcher on each side is still advancing after the restart
   (`avalon list-equivocations` should show no new unresolved findings).
   This only matters for the settlement signing key itself
   ([`key-rotation.md`](../for-maintainers/key-rotation.md)) — a routine
   code upgrade doesn't touch that key and shouldn't disrupt mirroring on
   its own, but it's worth confirming rather than assuming.

## Emergency security patch

Same steps as above, compressed — the only difference is urgency, not
mechanics:

1. Back up the database and `.env` anyway (step 1-2 above) — a rushed
   upgrade with no way back is how a patch turns into an outage.
2. `git fetch origin --tags && git checkout <the patch tag>` — a
   `Z`-level security patch ships as its own tagged release, same as any
   other release, per [`release-process.md`](../for-maintainers/release-process.md#hotfixes)
   (it may be cut from a short-lived hotfix branch behind the scenes, but
   what a hoster checks out is still a tag, never that branch directly).
3. `make stack-up`.
4. Verify (step 4 above), specifically confirming the vulnerability this
   patch addresses is actually closed, not just that the process is up.

## Rolling back

If the new version is broken:

1. **Stop the stack:** `make stack-down`.
2. **Check out the previous release tag:**

   ```bash
   git checkout "$(cat ./avalon-previous-release-tag.txt)"
   ```

3. **Restore the database if a migration ran and needs reverting.**
   `make migrate-down` (`cargo run -p avalon-server --bin migrate -- down`)
   reverts only the single most-recently-applied migration, one step at a
   time — run it repeatedly if you need to unwind more than one. For
   anything more involved (a migration that's destructive or hard to invert
   cleanly), restore the `pg_dump` backup from before the upgrade instead:

   ```bash
   docker cp ./avalon-backup-<timestamp>.dump avalon-postgres:/tmp/restore.dump
   docker exec avalon-postgres pg_restore -U avalon -d avalon --clean --if-exists /tmp/restore.dump
   ```

   Restoring a backup loses any data written between the backup and now —
   know which of the two options (step-down migrations vs. full restore) is
   correct for what actually broke before running either.

4. **Rebuild and restart on the old tag:** `make stack-up`.
5. **Verify** (same checks as a routine upgrade above) before considering
   the rollback complete.

## Rolling an upgrade out across a mirrored/multi-node network

There is no coordinated-rollout mechanism that pushes an upgrade to every
node at once, and there shouldn't be — no party can force any operator to
upgrade (see [`../architecture/nodes.md`](../architecture/nodes.md) on
decentralized version rollout). What exists instead is organic, and it's worth
understanding so you can actually use it rather than treating every node
as fully isolated:

- **Upgrading one node is visible to its direct peers automatically.**
  Once a node is running the new `protocol_version`, any peer that already
  has it in its own peer table (via `POST /nodes/announce`/DHT identify)
  sees the newer version the next time it checks `GET /nodes/status` on
  itself — `newest_known_peer_version` and `stale` (#368) are computed
  straight from that peer table, no extra step required on your part.
- **This does not chain automatically beyond direct peers, and it does
  not apply anything.** A node reports only what it has directly observed
  from its own peers — there's no relaying of "peer X told me peer Y is on
  version Z" onward to third parties (see #797 for why: an unverified
  claim like that, amplified network-wide, is a spoofable
  "manufacture fake urgency" vector, not a safe optimization). And per
  #308/#797, none of this is ever acted on automatically — every hop still
  needs a human to see the signal and decide to upgrade.
- **So the realistic rollout shape is:** upgrade a handful of key/seed
  nodes first (ones with the most peer connections — well-connected
  mirrors, nodes other hosters commonly mirror). Their direct peers pick up
  the new version signal, their operators upgrade in turn (on their own
  schedule, not forced), and awareness spreads hop-by-hop through the
  actual peer graph — genuinely faster the better-connected the network
  is, but bounded by how many operators are actually paying attention and
  choosing to act, not automatic or instant. #795 (opt-in public-feed
  check) helps here too, specifically for nodes with few or no peers —
  it's a second way to learn a release exists that doesn't depend on being
  connected to an already-upgraded peer at all.
- **Mixed versions coexisting for a while is expected, not a bug** — as
  long as everyone stays within the same `X` generation. Read what
  actually changed (step 2 of the routine upgrade above) before assuming
  it's safe to run mixed versions across a mirrored set for an extended
  period, and never assume it's safe across an `X` boundary at all until
  #797 (clean version-mismatch rejection) is decided and built.
- **Relaying beyond direct peers becomes safe once #798 lands** — a
  *signed* release manifest (#619's opt-in auto-update, gated on that
  signing key) can be relayed peer-to-peer indefinitely without the
  "unverified claim" problem above, since a forged one simply fails
  verification. Until then, awareness only travels as far as direct,
  first-hand peer observation.

Want to actually see this propagation happen instead of taking it on
faith? [`multi-node-testing.md`](../for-maintainers/multi-node-testing.md)
walks through standing up three local nodes and watching an upgrade
propagate hop by hop.

## Near-zero-downtime upgrades

`make stack-up`'s rebuild-and-restart is a brief-downtime swap, not
zero-downtime — fine for most self-hosted deployments, but if even that
few-second gap matters, run a manual blue/green swap using the reverse
proxy [`deployment.md`](deployment.md) already has you running in front of
`avalon-server`:

1. **Bring up the new version alongside the old one**, on a different
   local port. With the new tag checked out but *before* running the
   normal `make stack-up`, start a second `avalon-server` container
   pointed at the same Postgres, bound to e.g. `127.0.0.1:8081` instead of
   the existing `127.0.0.1:8080` (`docker compose run -d --name
   avalon-server-new -p 127.0.0.1:8081:8080 avalon-server`, or the
   equivalent for however your stack is customized). Migrations still need
   to run first (`make migrate` or the one-shot `migrate` container) —
   they apply to the shared database regardless of which container serves
   traffic, so this is not a way to avoid the migration itself, only the
   restart gap.
2. **Verify the new container directly** (`curl 127.0.0.1:8081/nodes/status`,
   or whatever check you'd normally run against a fresh start) before it
   ever sees real traffic.
3. **Flip the reverse proxy's upstream** from `8080` to `8081` (a one-line
   `Caddyfile`/nginx config change plus a reload — `caddy reload` or
   `nginx -s reload`, no proxy restart needed) once you're confident.
   Traffic moves to the new version with no gap; the old container is
   still running and available as an instant rollback target (flip the
   proxy back) if something looks wrong immediately after cutover.
4. **Tear down the old container** once you're confident the new one is
   healthy under real traffic: `docker stop`/`docker rm` the old
   `avalon-server` container, then let a subsequent `make stack-up` treat
   the new one as the normal one going forward (rename/re-tag it to match
   what `make stack-up` expects, or just let the next routine upgrade
   replace it the normal way).

This is a manual procedure today. Whether it's worth a `make` target to
assist the app-container half of this (never the reverse-proxy flip
itself — that stays yours either way) is tracked by #799, design not yet
settled. A real rolling-upgrade/multi-replica setup (a load balancer in
front of more than one `avalon-server` instance) isn't documented anywhere
because the default topology this project documents is single-node.
