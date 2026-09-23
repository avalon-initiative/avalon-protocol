# Testing a local multi-node network

Multi-node hosting is already real — `AVALON_MIRROR_PEERS`/
`AVALON_BOOTSTRAP_PEERS` and the mirror-watcher's backfill machinery
(see [`../architecture/settlement.md`](../architecture/settlement.md)) work
today. What's missing is a repeatable way to actually stand up more than
one node locally and watch it work — today that kind of testing mostly
happens ad hoc, against whatever sandbox hosts happen to be available. This
doc is that repeatable procedure: three local `avalon-server` processes on
one machine, mirroring each other, so anyone can validate mirroring, peer
discovery, and the version-awareness/upgrade-chain behavior
[`upgrading.md`](../for-hosters/upgrading.md#rolling-an-upgrade-out-across-a-mirroredmulti-node-network)
describes, without needing separate physical or virtual hosts.

Prerequisites: the same native toolchain
[`local-development.md`](../../../maintainers/local-development.md) already
requires (`cargo`, a reachable Postgres). This uses the native `cargo run`
path rather than `make stack-up`'s Docker path, since running several
instances at once is simpler as plain processes on different ports than as
several Docker containers.

## Why environment variables, not several `.env` files

`crates/devenv` loads the workspace root's single `.env` from a fixed path
([issue #671](https://github.com/LunarVagabond/avalon-protocol/issues/671))
via `dotenvy::from_path(...).ok()`, which — like `dotenvy` generally — never
overrides a variable **already set in the process's real environment**. That
means the reliable way to run several differently-configured nodes against
the same checkout is exporting the values that need to differ per node
directly in the shell before `cargo run`, not maintaining multiple `.env`
files that `devenv` has no way to choose between.

## 1. Create one database per node

Mirrors each keep their own local backfilled copy of the ledger (see
`settlement.md`'s "Postgres implementation") — each node needs its own
database, not a shared one:

```bash
psql "$DATABASE_URL" -c "CREATE DATABASE avalon_node1;"
psql "$DATABASE_URL" -c "CREATE DATABASE avalon_node2;"
psql "$DATABASE_URL" -c "CREATE DATABASE avalon_node3;"
```

(Adjust the connection string/role as needed for your local Postgres —
same `avalon`/`avalon` default `docker compose up -d postgres` already
gives you, if you're using that for the database itself while running
`avalon-server` natively.)

## 2. Migrate each database

```bash
for db in avalon_node1 avalon_node2 avalon_node3; do
  DATABASE_URL="postgres://avalon:avalon@localhost:5432/$db" make migrate
done
```

## 3. Start three nodes, each mirroring the other two

All three share the same `AVALON_NETWORK_ID` (they're mirrors on one
network, not three separate networks) and each gets its own port, database,
and signing key:

```bash
# Node 1 — authority
AVALON_SERVER_ADDR=127.0.0.1:8080 \
DATABASE_URL="postgres://avalon:avalon@localhost:5432/avalon_node1" \
AVALON_NETWORK_ID=avalon-multi-test \
AVALON_SETTLEMENT_SIGNING_KEY=$(python3 -c 'import secrets; print(secrets.token_hex(32))') \
AVALON_MIRROR_PEERS=http://127.0.0.1:8081,http://127.0.0.1:8082 \
cargo run -p avalon-server &

# Node 2 — mirror
AVALON_SERVER_ADDR=127.0.0.1:8081 \
DATABASE_URL="postgres://avalon:avalon@localhost:5432/avalon_node2" \
AVALON_NETWORK_ID=avalon-multi-test \
AVALON_MIRROR_PEERS=http://127.0.0.1:8080,http://127.0.0.1:8082 \
cargo run -p avalon-server &

# Node 3 — mirror
AVALON_SERVER_ADDR=127.0.0.1:8082 \
DATABASE_URL="postgres://avalon:avalon@localhost:5432/avalon_node3" \
AVALON_NETWORK_ID=avalon-multi-test \
AVALON_MIRROR_PEERS=http://127.0.0.1:8080,http://127.0.0.1:8081 \
cargo run -p avalon-server &
```

Only node 1 gets `AVALON_SETTLEMENT_SIGNING_KEY` — it's the shard's
authority; nodes 2 and 3 are pure mirrors, matching the "Node roles at a
glance" table in [`hosting-quickstart.md`](../for-hosters/hosting-quickstart.md).
Every other `AVALON_*` value not set here keeps its `.env.example` default.

## 4. Verify the mesh actually formed

```bash
curl -s http://127.0.0.1:8080/nodes/peers | jq
curl -s http://127.0.0.1:8081/nodes/status | jq
```

Each node's `/nodes/peers` should list the other two once
`AVALON_ANNOUNCE_INTERVAL_SECS` (default 180s) has had a chance to run at
least once — don't expect this to populate instantly on startup.

## 5. Prove mirroring actually replicates, not just discovers

Write something through node 1 (e.g. `avalon create-identity` against
`http://127.0.0.1:8080`), then confirm it shows up on nodes 2 and 3's own
ledgers once their mirror-watcher backfills it:

```bash
AVALON_SERVER_ADDR=http://127.0.0.1:8081 cargo run -p avalon-cli --bin avalon -- inspect-ledger
AVALON_SERVER_ADDR=http://127.0.0.1:8082 cargo run -p avalon-cli --bin avalon -- inspect-ledger
```

## 6. Use this topology to test the upgrade-chain claim

This is the actual point of writing this doc down: `upgrading.md` and
`release-process.md` both describe an organic, gossip-carried
update-awareness chain (#368/#795) rather than a central push. This
topology is small enough to watch that claim happen instead of trusting it
on paper:

1. Stop node 1, check out a different commit (a real version bump — even
   just editing `version.workspace` in `Cargo.toml` locally is enough to
   simulate one), rebuild, and restart it on the same port.
2. Poll node 2's or node 3's `/nodes/status` — `newest_known_peer_version`
   should pick up node 1's new version on their next `AVALON_ANNOUNCE_INTERVAL_SECS`
   cycle, and `stale` should flip to `true`.
3. Confirm it does **not** propagate further than that — a fourth node that
   only peers with node 2 (not node 1 directly) should *not* see node 1's
   version in its own `newest_known_peer_version` unless node 2 also gets
   upgraded. This is the "no relay beyond direct peers" behavior #797
   documents as deliberate, not a gap — worth actually seeing it hold
   before relying on it.

## Tearing down

```bash
kill %1 %2 %3   # or fg/Ctrl-C each if run in separate terminals
for db in avalon_node1 avalon_node2 avalon_node3; do
  psql "$DATABASE_URL" -c "DROP DATABASE $db;"
done
```

## Scaling this beyond one machine

Everything above works identically across separate hosts — swap
`127.0.0.1:808x` for each host's real reachable address (see
[`deployment.md`](../for-hosters/deployment.md) once any of them are
reachable beyond loopback), and each host runs its own Postgres instead of
sharing one. The mechanics don't change; only "how many hosts is enough to
show real behavior" does — a topology genuinely needs enough nodes to make
a scenario non-degenerate (peer-set growth past bootstrap, gossip fan-out,
shard discovery across more than a couple of nodes) before three tells you
much beyond "mirroring works at all."
