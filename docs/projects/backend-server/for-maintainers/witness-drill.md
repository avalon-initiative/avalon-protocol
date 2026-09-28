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
| `lifecycle` | A alone, then A,B,C, then A removed (B,C), then B,C,D,E. Checks admission with no restart of A, that B and C keep answering, that exactly A's witness key (and no other slot) leaves B's list once A is gone, and that D and E are admitted without A. | yes |
| `eclipse` | A victim is announced to by a flood of nodes that all share one /24 (every loopback node does). Asserts the list fills up to the per-prefix cap and never past it. | yes (4-node flood) |
| `witness-loss` | A known-list member is frozen (`SIGSTOP`) without leaving anyone's peer list. Asserts it is dropped once past the freshness window and a newly joined node takes the slot. | no |
| `long-offline` | A mirroring node is stopped, a write lands elsewhere, ten seconds pass (well past the sped-up windows), and it restarts on the same schema. Asserts it rejoins the peer table. | no |
| `fork` | Two nodes author the same shard with the same key and accept different writes, producing a genuine fork. A third node mirrors both and must log and record `equivocation_detected`. A fourth node mirrors nothing and only hears both heads over announce gossip; both authors are bare (no cosignatures), so it must confirm the fork from the two author signatures and store `author` evidence with no witness named (read back with the `witness_evidence` example). | no |
| `rollout` | A single-key author writes history; two mirrors start with witness keys and cosign it. Asserts history is unchanged, the default head body is the same shape, an old single-key client verifies before, during and after, a witness-aware client accepts the head with both cosignatures and rejects it with one, the log keeps growing, and turning cosigning off on one node changes nothing else. See [`witness-policy-rollout.md`](./witness-policy-rollout.md). | no |
| `cosigned` | One author, three cosigning witnesses and one non-cosigning mirror. The mirror's known list holds exactly the three witness keys, all confirmed; a head written afterwards reaches the mirror only through cosignatures; a witness-aware client accepts it by majority whether it reads the mirror or the witnesses; one witness is killed, verification continues on the other two, its slot is dropped and a newly started witness takes it. |

The CI subset is the `witness-drill-smoke` job in `.github/workflows/ci.yml`
(`lifecycle` and a 4-node `eclipse`, about half a minute of scenario time). `fork` and `cosigned` start four to six nodes each and take a minute or more apiece, so they stay local.

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
8. Unchanged head: leave the `core` head unchanged for well over the freshness
   window and sample `curl "<ip>:8080/ledger/sth/latest?shard_id=core&witnesses=1"`
   on each mirror every minute or so. The `observed_at` of every confirmed
   known-list witness's cosignature should stay within one re-attest interval
   plus one mirror-watcher tick (about 320 seconds by default), and feeding the
   response to `verify_sth cosigned <known-list keys>` (with the pinned
   `AVALON_SETTLEMENT_VERIFY_KEY`) should keep returning success. Repeat with
   an outbound firewall drop from a mirror to its source (not a changed
   `AVALON_MIRROR_PEERS`, which changes the source identity the mirror serves
   under) to show refresh does not depend on the author being reachable. A witness that is not in a
   mirror's confirmed known list is not refreshed there and its copy ages out.
   A restarted node's list may need its probation window before it gathers.

After a drill, restore the fleet to its previous state and record what was
run and the results in the epic.

### Tools used for the observations

- Known list: `data/known_list.json` on each node (`witness_key_id`, `status`, `prefix`).
- Held cosignatures and their age: `GET /ledger/sth/latest?shard_id=<shard>&witnesses=1`
  (`observed_at` per cosignature).
- Client-style majority check that gathers each witness's own cosignature from that witness
  and feeds them to `target/debug/examples/verify_sth cosigned <witness-key-hex>...`
  (needs `AVALON_SETTLEMENT_VERIFY_KEY` set to the pinned key).
- The Rust SDK's `verify_network()` and zero-URL `discover()` from a scratch binary, and its
  `verify_network_with_policy(Explicit(..))` with a chosen witness list.
- Self-certifying shard check: the node's own shard id equals `node:` plus the sha256 of its
  settlement public key, and `verify_sth old` accepts its head under only that key.
- Registrations with `avalon create-identity` (a signed `identity.created` event) and
  `avalon login` against each node, with `AVALON_SERVER_URL` and `AVALON_WEBAUTHN_ORIGIN` set.

## Live drill record, 2026-09-28

Fleet: network `avalon-dev-lan`, five nodes on one /24, main at 5f71364 (peers rebuilt fresh
that day). Roles: 192.168.7.113 authors the pinned `core` and runs no witness role in any peer's
list; .174, .183, .204 each author their own self-certifying `node:` shard and cosign; .194 was
reinitialised mid-drill. Every node held a known list of two (see limits), all `confirmed` by
about 12:57Z. Timings below are UTC.

1. Baseline, 12:43 to 13:05. All five nodes healthy; every witness re-attested its own
   cosignature of the `core` head about every 200 seconds (ages seen at 29, 53, 117 seconds and
   so on, resetting between samples; two or more intervals on each). After a write on core the
   head advanced 1 to 4 and every mirror converged on the same root. The client-style gather
   check accepted the head for two and for four witnesses.
2. Original node stopped, 13:05:15 to 13:31:52 (26 min 37 s). The four survivors answered
   `/nodes/status`, kept a full peer set among themselves and kept re-attesting (every node's own
   cosignature stayed under 200 seconds old for the whole window). Registration
   (`identity.created`, signed by the identity key) and `login` succeeded on each survivor's own
   shard, growing each shard's log from 3 to 6 entries. Survivors' heads verified under the
   witness policy: the client-style gather check returned exit 0 for two and four witnesses, and
   the Rust SDK `verify_network_with_policy(Explicit([c6137167, 5d31f6ec]))` returned `Verified`
   against .204 (see findings for the default `Auto` list, which returned `Mismatch`).
3. Fresh node with no authority online, 13:08:58 onward (original still down). .194 was
   stopped, its database reset, its data directory removed and its `.env` replaced with only
   the network id, its own addresses, the pinned public verify key, and a single surviving peer
   (.204) as both bootstrap and core mirror. On first boot it generated its own settlement,
   submit, witness and libp2p keys and chose the self-certifying shard
   `node:07ea8656...`. Within four seconds it announced, learned the other three peers and their
   shards by gossip, mirrored the `core` head from .204 (verified against the pinned key) and
   cosigned it. A registration on it wrote to its own shard (3 entries); the shard id equals
   the sha256 of its public key and its head verifies under that key alone. It filled a known
   list of two from discovery (one confirmed after a 60 second probation set for the drill) and
   appeared as a witness in the other nodes' peer tables. Its log shows two indexer projection
   errors when replaying mirrored core entries (foreign key on the identity row), which the node
   itself reports as expected for a replay-only node.
4. Original node restarted, 13:31:52. Its peer table held all four peers within a minute. A
   registration on it advanced core from 4 to 7 entries; by 13:35 every survivor and the fresh
   node had mirrored root `5ed1919e...` at tree size 7 and gathered a full set of fresh
   cosignatures. No node logged `equivocation_detected`, and the primary's own log had none. With
   the head fresh the Rust SDK `verify_network()` returned `Verified` for .204 and .174 and
   zero-URL `discover()` selected .204.

### Findings and limits

- What is demonstrated: the network kept serving, gossiping, re-attesting and accepting
  registrations and signed actions on the survivors' own shards with the original down; a
  freshly initialised node joined and authored with no authority reachable; the original
  rejoined ordinarily and converged with no fork or evidence.
- A `core` head that stays unchanged while its author is away stops verifying from a mirror after
  about ten minutes, and the SDK `Auto` policy returned `Mismatch` on every survivor during the
  outage: a mirror only gathers other witnesses' cosignatures inside a tick that first fetched
  the source head, and merging keeps a stale relayed copy over a witness's own fresh one.
  Tracked in #1025 (a candidate change and its live result are recorded there; it did not fully
  repair the symptom).
- Self-certifying `node:` shards were authored and served but nobody else could mirror or verify
  them (mirroring resolved only registered or pinned keys), so they had no mirrors and no
  witnesses, and only `core` had cosigned heads. Fixed in #1026: a node with
  `AVALON_MIRROR_ALL_DISCOVERED_SHARDS=true` verifies the key the head carries, pins it, mirrors the
  shard and cosigns it, and the per-tick cosignature refresh covers pinned shards.
- `core` itself has a single writer: with the original down, core could not grow and new
  integrators or registered shards could not be created. Only self-certifying shards kept
  authoring. #824 covers the cross-shard root with an unreachable core authority.
- The fleet shares one /24, so the per-prefix cap of 2 limits every known list to two
  witnesses and majority is both of them. It cannot show lists above two, prefix diversity or
  the diversity-cap eclipse behaviour; that needs hosts on several subnets. The fresh node also
  cannot enter the other nodes' lists while two same-prefix slots are held.
- The original was never a slot in any known list (it advertises no witness role in the peer
  tables), so pruning of the original's slot after the freshness window was not exercised on the
  fleet; the local `lifecycle` scenario covers it.
- Not run on the fleet this time: witness loss with a frozen witness, a long-offline rejoin,
  the fork scenario and the rollout rollback (`AVALON_WITNESS_COSIGNING_ENABLED=false`); an
  earlier fleet run is recorded in [`witness-policy-rollout.md`](witness-policy-rollout.md).
