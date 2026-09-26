# Rolling a network onto witness cosigning, and back

How to move a running single-key network (the dev fleet, the integration
environment, a future production network) onto witness cosigning without a reset,
what old and new clients see at each step, and how to undo it. Design:
[`witness-cosigning.md`](../architecture/witness-cosigning.md). Trust-anchor
background: [`network-trust-anchors.md`](../architecture/network-trust-anchors.md).

## What changes and what does not

Nothing about the log itself changes. The authoring node keeps its settlement key and
keeps signing every tree head exactly as before; history is never rewritten and no
entry is re-signed. Witness cosigning only adds signatures made by other nodes over
heads that already exist.

| | Before | After |
|---|---|---|
| Who signs a head | the shard's author key | the same, plus witnesses' cosignatures stored beside it |
| `GET /ledger/sth/latest` body | STH fields | identical; `?witnesses=1` adds a `cosignatures` array |
| Client pinned to one key, checks the author signature | verifies | verifies, unchanged |
| Node verifying mirrored heads | author signature | majority of its own known witnesses, which is the author signature when it knows zero or one witness |
| Database | migrations up to 0072 | additive migrations `0073` to `0077` (cosignatures, shard scoping, name claims, equivocation evidence, witness checkpoints); no existing table is rewritten |

The pinned key in `docs/trusted-networks.json` does not change and no client needs
updating for a node to start cosigning. Clients that verify cosignatures themselves do
not exist yet (SDK work, avalon-initiative/avalon-sdks#37), so until they ship the
assurance a pinned client gets is what it always was. The value of rolling out first is
that mirrors and cross-shard verification start checking majorities and recording
equivocation, and that the network is exercising the design before clients depend on it.

## Preconditions

- Every node runs a build with the witness code (cosigning decision, witness-key
  adverts, shard-scoped cosignature storage). Check `GET /nodes/status` (`protocol_version`) on each.
- A backup of each node's database ([`upgrading.md`](../for-hosters/upgrading.md)).
- At least two nodes that will be witnesses besides the author, so a majority means
  something. With one witness the rule reduces to the author signature; that is safe,
  just not stronger.
- Do not `db-reset` INT, staging or production for this. Nothing here needs it: the
  migrations are additive. (The dev fleet may be reset while it is being actively worked.)

## Procedure

Do the author last. Every step is reversible, see Rollback.

1. **Upgrade and migrate each node**, mirrors first, the shard authors and the core
   authority last, following the rolling procedure in
   [`upgrading.md`](../for-hosters/upgrading.md). `make migrate` applies `0073` to `0077`.
   A node with no witness key configured behaves exactly as before.
2. **Give each intended witness a key** and restart it with cosigning left on:
   `AVALON_WITNESS_SIGNING_KEY=<32-byte hex seed>` (a node that authors a shard can rely
   on its settlement key instead). Keep `AVALON_MIRROR_PEERS` pointed at the authorities
   whose heads it should cosign; a node only cosigns heads it mirrors.
3. **Let the known lists fill.** Each node admits peers that prove a witness key in their
   own announce response, with a 30-minute probation before a new slot counts. Nothing to
   configure; the bundled seed nodes are anchors. Watch `known_list` log lines
   (`known_list_tick`) and `<AVALON_DATA_DIR>/known_list.json`.
4. **Verify** (below).
5. **Update the trust-anchor entry and released SDK policy in step** only once the SDKs
   verify cosignatures: at that point the release notes carry the new client policy and
   `trusted-networks.json` keeps the same pinned key and seed nodes. Until then this step
   is a no-op, deliberately: pinned clients keep verifying by the key.

Roll the tiers in order, waiting for a clean verification on each before the next: the
dev fleet, then INT, then production. INT and production take no resets for this.

## Verify

Against any node, with the network's verify key in the environment:

```
curl -s http://NODE/ledger/sth/latest | target/debug/examples/verify_sth old
curl -s "http://NODE/ledger/sth/latest?shard_id=core&witnesses=1"
```

The first is what a pinned client does and must exit 0 before, during and after. The
second lists the cosignatures that node holds. To check a majority the way a
witness-aware client will, feed the same head as served by each witness to
`verify_sth cosigned <witness-key-hex>...`; it accepts only if a majority of the given
keys cosigned, and rejects the same head with one of two. The author serves no
cosignatures itself; a mirror collects them from each confirmed witness's
`GET /ledger/sth/{tree_size}?shard_id=<shard>&witnesses=1`, so a mirror that logs
"holding it" for a head is waiting for a majority of witnesses to answer for that exact
head, not failing verification. Also confirm:

- `GET /ledger/sth/{tree_size}` on the author returns the same root and signature it did
  before the rollout for a size that existed then (history unchanged);
- `GET /ledger/proof/consistency?first=<old>&second=<new>` is served across the rollout;
- the default `latest` body has no `cosignatures` key.

`scripts/witness-drill.sh rollout` performs every one of these against real processes
(`make witness-drill SCENARIOS=rollout`).

## Rollback

There is nothing to undo in the log. To stop a node cosigning, set
`AVALON_WITNESS_COSIGNING_ENABLED=false` and restart it: it keeps verifying and serving
what it already stored, produces no new cosignatures, and its peers simply see one fewer
witness once its slot goes stale. To take every witness off, do that on each. A node can
also be rolled back to the previous build: the additive tables are ignored by older
code, and `make migrate-down` reverses `0077` back to `0073` one step at a time if a
schema rollback is ever required (do not use it on INT or production without a backup).
Turning cosigning back on later resumes from the node's stored checkpoint; it will not
sign a head that does not extend it.

## Rehearsal

- Local, isolated, real processes: `make witness-drill SCENARIOS=rollout`. It starts a
  single-key author, writes history, starts two cosigning mirrors, checks the list above,
  keeps writing, then turns one witness off and repeats the old-client and history checks.
  See [`witness-drill.md`](./witness-drill.md). Result on the release that introduced
  this runbook: 25 of 25 checks pass, twice in a row, including the rollback half (the
  disabled node still mirrors, produces no cosignature, and every earlier head still
  verifies).
- Dev fleet: the five-node live drill (main at d3adb60) passes 55 of 56 checks. It covers a
  baseline with all five nodes, growth from two nodes, loss of the original node (including a
  brand-new node authoring under a self-certifying shard id while the original is down),
  witness loss with a real write while one witness is frozen, a long-offline rejoin and prefix
  diversity. The one failing line measured a retired shard name after a node was reconfigured,
  not a product fault. The drill also found and fixed two real problems: an unbounded HTTP
  client let a hung peer stall a node's mirror loop, and cosignatures on an unchanged head aged
  out after the freshness window so a quiet network's head stopped verifying. Witnesses now
  re-attest their current head on an interval, and a head with no writes for over ten minutes
  stayed verifiable on the fleet.

## Limits worth knowing

- Cosignatures only reach a node's known list through peers that prove a witness key in
  their own announce response, and a new slot waits out probation, so a freshly started
  witness is not counted for about half an hour.
- Client-side cosignature verification and a witness list in the trust-anchor entry are
  not shipped; both need the coordinated SDK release.
