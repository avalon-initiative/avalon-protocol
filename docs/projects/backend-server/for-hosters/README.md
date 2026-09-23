# For Hosters

Documentation for people standing up an `avalon-server` node to actually run
it — for their own community, a game, or just to see it work — not for
people contributing code to this repository itself (see
[maintainers docs](../../../maintainers/)) or integrating Avalon into their own
game/app/service as a developer (see [developer docs](../../sdks/rust/for-developers/)).

No Rust or Node toolchain is required for anything on this page — only
[Docker](https://docs.docker.com/get-docker/).

## Start here

1. [`hosting-quickstart.md`](hosting-quickstart.md) — the fastest path from
   a fresh checkout to a running node: `make stack-up`, one command, safe
   defaults for everything except the two values that genuinely can't have
   a shared one (the settlement signing key and network id, both generated
   for you on first run).
2. [`deployment.md`](deployment.md) — required reading before that node is
   reachable from anywhere other than `127.0.0.1`: TLS termination via a
   reverse proxy (Caddy or nginx), example configs, and which env vars need
   real production values instead of local-dev defaults.

## If something goes wrong

Both guides above have their own troubleshooting section for the common
first-run failures. Beyond that:

- If your node watches peers (`AVALON_MIRROR_PEERS` set) and its
  mirror-watcher reports equivocation, see
  [`equivocation-response.md`](../for-maintainers/equivocation-response.md).
- To rotate the settlement signing key itself — routine hygiene or a
  suspected compromise — see
  [`key-rotation.md`](../for-maintainers/key-rotation.md).

## Background

[`../architecture/self-hosting.md`](../architecture/self-hosting.md) and
[`../architecture/nodes.md`](../architecture/nodes.md) cover the concepts
behind what these guides walk through — what a node's roles mean today,
running a private instance versus joining the public network, and the
combined-binary default versus the multi-role topology available today —
if you want the "why," not just the "how."
