# For Hosters

Documentation for people standing up an `avalon-server` node to actually run
it — for their own community, a game, or just to see it work — not for
people contributing code to this repository itself (see
[maintainers docs](../../../maintainers/)) or integrating Avalon into their own
game/app/service as a developer (see [developer docs](https://github.com/avalon-initiative/avalon-docs/blob/main/integrations/README.md)).

The Docker route needs only [Docker](https://docs.docker.com/get-docker/); the
standalone-binary route needs only a Postgres database.

## Start here

0. [`standalone-binary.md`](standalone-binary.md) — run the `avalon-server`
   binary directly: start with `avalon setup`, a guided, idempotent first-run
   flow (also non-interactive with `--yes`), then configuration for a replica or
   a shard-authoring node, first start, a systemd unit, upgrading, backup and
   troubleshooting, for both the plain and the bundled-database
   (`avalon-server-bundled`) variant. These guides are embedded in the `avalon` binary:
   `avalon guide [topic]`.

1. [`hosting-quickstart.md`](hosting-quickstart.md) — the fastest path from
   a fresh checkout to a running node: `make stack-up`, one command, safe
   defaults for everything except the two values that genuinely can't have
   a shared one (the settlement signing key and network id, both generated
   for you on first run).
2. [`deployment.md`](deployment.md) — required reading before that node is
   reachable from anywhere other than `127.0.0.1`: TLS termination via a
   reverse proxy (Caddy or nginx), example configs, and which env vars need
   real production values instead of local-dev defaults.
3. [`upgrading.md`](upgrading.md) — once that node is live: backing it up,
   rolling out a routine upgrade or an emergency security patch, and
   rolling back if a new version breaks something.

4. [`choosing-your-shard.md`](choosing-your-shard.md) — which shard your node
   authors (`core` is reserved for the network's pinned core authority), how
   to get a registered shard key, the startup guard's errors, and running one
   shard with hot standby or sibling shards.
5. [`seed-nodes.md`](seed-nodes.md) — how seed nodes are run and monitored,
   what a fresh node does with a network's seed list, and how a trust entry
   changes over time.
5. [`verifying-a-release.md`](verifying-a-release.md) — checking the checksum and
   build provenance of a prebuilt release download before running it.
6. [`running-without-an-open-port.md`](running-without-an-open-port.md) — a node
   with no inbound reachability: what it needs, what works over its outbound
   connections, and the limits.

## If something goes wrong

Both guides above have their own troubleshooting section for the common
first-run failures. Beyond that:

- If your node watches peers (`AVALON_MIRROR_PEERS` set) and its
  mirror-watcher reports equivocation, see
  [`equivocation-response.md`](../for-maintainers/equivocation-response.md).
- If the node that authors a shard has failed and a mirror has to take over,
  see [`authority-promotion.md`](../for-maintainers/authority-promotion.md).
- To rotate the settlement signing key itself — routine hygiene or a
  suspected compromise — see
  [`key-rotation.md`](../for-maintainers/key-rotation.md).

## Background

[`avalon-docs: architecture/self-hosting.md`](https://github.com/avalon-initiative/avalon-docs/blob/main/architecture/self-hosting.md) and
[`avalon-docs: architecture/nodes/README.md`](https://github.com/avalon-initiative/avalon-docs/blob/main/architecture/nodes/README.md) cover the concepts
behind what these guides walk through — what a node's roles mean today,
running a private instance versus joining the public network, and the
combined-binary default versus the multi-role topology available today —
if you want the "why," not just the "how."
