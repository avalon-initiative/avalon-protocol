# Seed nodes and trust entries

A seed node is an ordinary always-on node whose base URL is listed in a
network's entry in `docs/trusted-networks.json`. It carries no authority: the
entry's `verify_key` is the only trust root, and every tree head a node
receives, from a seed or anywhere else, is verified against it.

## What a fresh node does with the list

- Announces itself to the entry's `seed_nodes` and learns the rest of the
  network through gossip. `AVALON_BOOTSTRAP_PEERS` replaces the list.
- If it does not author `core` and `AVALON_MIRROR_PEERS` is unset, it mirrors
  `core` from the same seed nodes. `AVALON_MIRROR_PEERS` always wins, and
  `AVALON_CORE_MIRROR_SEEDS` (comma-separated URLs) overrides only this
  default. A seed that serves a wrong or forged head is rejected by the pinned
  key, so it can withhold history but cannot inject any.
- An entry with an empty `seed_nodes` (`avalon-dev-local`, used by local and CI
  nodes) changes nothing: no default mirror source is applied.

A replica-only node needs no `AVALON_WEBAUTHN_RP_ID` or
`AVALON_WEBAUTHN_ORIGIN`; any node that serves logins still requires both.

## Running a seed node

Run it as a normal node with a stable public address that is set as its
`AVALON_NODE_URL`, and keep it on the release binary matching the rest of the
network. Prefer a replica-only seed so it holds no signing key. It should
mirror `core`, so joiners get a verifiable head from it.

Monitor each seed from outside its own network:

- `GET /nodes/status` answers with the expected `network_id`.
- `GET /ledger/sth/latest` returns a head that verifies against the entry's
  key and whose `tree_size` keeps up with the authority.
- `GET /nodes/discover` lists other nodes, so a seed that knows only itself is
  isolated.

An unreachable seed only slows joining while the other listed seeds answer, so
list several, on different hosts and networks.

## How an entry changes

The file is versioned in this repository and changes go through normal review:

- Adding or removing a seed is a pull request that edits `seed_nodes`. Add the
  new node and confirm it passes the checks above before merging; remove a
  node from the list before decommissioning it.
- Rotating `verify_key` is a coordinated change across every node and SDK
  release that bundles the file; clients on an older build keep the old key.
- A node reads the list compiled into its binary, so a change reaches operators
  with the next release. Operators who need a different list sooner set
  `AVALON_BOOTSTRAP_PEERS` and `AVALON_MIRROR_PEERS`.

## Current entries

`avalon-dev-lan` is the maintainers' private-network development test bed. Its
seed addresses are not reachable from the internet, and it makes no
availability promise. Public seed nodes are tracked in #994. The full hosting
guide for the standalone binary is tracked separately.
