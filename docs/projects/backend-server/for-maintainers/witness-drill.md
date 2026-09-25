# Witness-cosigning drill

A scenario suite that grows a network from one real `avalon-server` process
to several, removes nodes including the original, drops and refills a
known-list witness, floods a victim with same-prefix candidates, forks a log,
and reconnects a node that was offline for a long stretch. The property under
test is in [`../architecture/witness-cosigning.md`](../architecture/witness-cosigning.md):
the network behaves the same at one node and at many, and survives losing any
operator's nodes, including the original.

The suite drives real processes against real Postgres. The unit-level
properties (majority intersection, `KnownList` admission, equivocation
proofs) have their own tests and are not repeated here.

## Running it locally

```bash
scripts/witness-drill.sh                 # every scenario
scripts/witness-drill.sh lifecycle eclipse   # a subset
make witness-drill SCENARIOS="fork"
```

Needs `jq`, a `.env` with a reachable `DATABASE_URL` (or `AVALON_ENV_FILE`),
and the debug server binary. Every node binds `127.0.0.1`, gets its own
Postgres schema (dropped on exit) and its own `AVALON_DATA_DIR`, so nothing
touches a running node or the default schema. The script reuses
`scripts/lib/harness.sh`, the same node-launch helpers `live-tests.sh` and
`load-tests.sh` use. Known-list state is observed through each node's
persisted `known_list.json`, and admission, pruning and refill are sped up
with the existing `AVALON_KNOWN_LIST_*` and `AVALON_ANNOUNCE_INTERVAL_SECS`
settings (including a raised `AVALON_KNOWN_LIST_FRESHNESS_FLOOR`, so a small list's scaled window stays above the 2 s refill interval); only the timings change, never the rules.

| Scenario | What it does | In CI |
|---|---|---|
| `lifecycle` | A alone, then A,B,C, then A removed (B,C), then B,C,D,E. Checks admission with no restart of A, that B and C keep answering and re-shape their lists after A is gone, and that D and E are admitted without A. | yes |
| `eclipse` | A victim is announced to by a flood of nodes that all share one /24 (every loopback node does). Asserts the list fills up to the per-prefix cap and never past it. | yes (4-node flood) |
| `witness-loss` | A known-list member is frozen (`SIGSTOP`) without leaving anyone's peer list. Asserts it is dropped once past the freshness window and a newly joined node takes the slot. | no |
| `long-offline` | A mirroring node is stopped, a write lands elsewhere, ten seconds pass (well past the sped-up windows), and it restarts on the same schema. Asserts it rejoins the peer table. | no |
| `fork` | Two nodes author the same shard with the same key and accept different writes, producing a genuine fork. A third node mirrors both and must log and record `equivocation_detected`. A fourth node mirrors nothing and only hears both heads over announce gossip; both authors are bare (no cosignatures), so it must confirm the fork from the two author signatures and store `author` evidence with no witness named (read back with the `witness_evidence` example). | no |
| `rollout` | A single-key author writes history; two mirrors start with witness keys and cosign it. Asserts history is unchanged, the default head body is the same shape, an old single-key client verifies before, during and after, a witness-aware client accepts the head with both cosignatures and rejects it with one, the log keeps growing, and turning cosigning off on one node changes nothing else. See [`witness-policy-rollout.md`](./witness-policy-rollout.md). | no |

The CI subset is the `witness-drill-smoke` job in `.github/workflows/ci.yml`
(`lifecycle` and a 4-node `eclipse`, about half a minute of scenario time).

## Not yet covered

- `fork` covers source-based detection and gossip-driven author-level
  confirmation. It does not cover a cosigned-majority proof across two
  disjoint witness groups (witness-level evidence from gossip), which is only
  unit-tested.
- Only `rollout` runs with real cosignatures (two cosigning mirrors of a
  single-key author). The other scenarios prove admission, loss, refill and
  diversity, and every drill node advertises its own witness key so the known
  lists can fill.
- The eclipse scenario floods from one prefix. Prefix diversity across many
  real prefixes needs hosts on different subnets, i.e. the fleet.

## Running it as a live drill on the dev fleet

The dev fleet (primary plus `avalon-peer`, `-two`, `-three`, `-four`; all
disposable Proxmox LXC containers) is normally left off. The same scenarios,
at real addresses and real timings:

1. Bring it up: `make start` on the primary, then on each peer
   `ssh <peer> 'export PATH=$HOME/.cargo/bin:$PATH; cd ~/avalon-protocol && git fetch -q origin && git checkout -q -B main origin/main && cargo build -q --workspace && make migrate && make start'`.
   Confirm with `curl <ip>:8080/nodes/discover` on all five nodes. After any
   database reset, re-register the integrator and each node's shard key first.
2. Growth: stop all but the primary and one peer, then start the rest one at
   a time. On each node, `AVALON_DATA_DIR/known_list.json` should gain the
   new member at the next refill tick (default 120 seconds) without any
   restart.
3. Loss of the original: `make stop` on the primary. `curl <ip>:8080/nodes/status`
   on the remaining nodes must still answer, registration and signed actions
   on the shard-authoring peers must still succeed, and after the freshness
   window (10 minutes by default) the primary's slot disappears from each
   known list and refills from discovery. Bring the primary back afterwards
   to show a returning original is an ordinary join. Repeat removing further
   nodes (A, then A,B,C, then B,C, then B,C,D,E, then C,D,E in the ticket's
   order).
4. Witness loss: `ssh <peer> 'kill -STOP $(pgrep avalon-server)'`, wait past
   the freshness window, check the slot is refilled, then `kill -CONT`.
5. Eclipse: the fleet spans few prefixes, so this is a check that the cap
   holds when several fleet nodes share a subnet: compare each known list's
   `prefix` counts against `AVALON_KNOWN_LIST_MAX_PER_PREFIX`.
6. Long-offline: stop a peer for longer than the freshness and probation
   windows while writes continue elsewhere, restart it, and confirm it
   catches up through its mirror-watcher and rejoins known lists.
7. Fork: only on a network you are prepared to wipe. Configure two nodes to
   author the same shard with the same key, submit different writes to each,
   and point a third node's `AVALON_MIRROR_PEERS` at both. Look for
   `equivocation_detected` in its log and a row in `equivocation_findings`
   (`avalon list-equivocations`). See
   [`equivocation-response.md`](equivocation-response.md) for handling.

A live drill should be repeated once cosigning-decision logic has landed and
before the witness-cosigning epic closes, with `known_list.json` sizes above
one on every node so majority verification, not the single-signer case, is
what is being exercised. After a drill, restore the fleet to its previous
state (off and freshly reset if that is how it was left) and record what was
run and the results in the epic.
