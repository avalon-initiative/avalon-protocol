# Promoting a mirror after a settlement authority outage

What to do when the node that authors a shard's Signed Tree Heads (its
settlement authority) is down and not coming back soon. A mirror keeps
serving reads from its own verified copy, but it never becomes a writer on
its own: **promotion is a manual, operator-driven procedure, deliberately
never automatic.** An election mechanism would reintroduce the
multi-writer consensus this network decided against (see
[`../architecture/settlement.md`](../architecture/settlement.md)), and the
failure it guards against, two nodes both signing for one shard, is exactly
what [`equivocation-response.md`](equivocation-response.md) exists to clean
up after.

This runbook is written against the shared `core` shard, whose loss pauses
new identity, social, and guild actions network-wide. An integrator's own
dedicated shard follows the same procedure with `--shard-id <id>`; its
blast radius is only that integrator's new issuances.

## Read this first: what exists and what does not

| Step | Status |
|---|---|
| Verify a mirror holds the authority's full history | Built: `avalon verify-mirror-convergence` |
| Rotate the signing key / update what verifies it | Documented: [`key-rotation.md`](key-rotation.md) |
| Rebuild projections from the ledger | Built: `avalon rebuild-index` |
| **Seed the new authority's ledger from a mirror's `mirrored_entries`** | Built: `avalon promote-mirror` |

A mirror stores the authority's entries in `mirrored_entries`, a table that
is deliberately separate from the `ledger_entries` an authority writes. The
authority also needs the batch records and its Signed Tree Head history;
`avalon promote-mirror` rebuilds all of that into a fresh database (step 4).
Two ways to get a working ledger onto the replacement host:

- **The authority's data is gone**: use `avalon promote-mirror` from a
  converged mirror (step 4). It is the only path that works from the
  mirror's copy alone. Do not hand-write rows into `ledger_entries`; a wrong
  batch root or sequence produces a ledger that verifies against nothing.
- **The failed authority's database or a backup of it survives**: restoring
  it onto the replacement host is the alternative. It keeps everything the
  authority held, including entries no mirror saw, and the mirror's job is
  only to prove the restored data matches what the network already saw
  (step 2). Skip the promotion command in step 4 and continue with
  its remaining items.

## Step 0: decide it is an outage, not a partition

Promoting while the old authority is merely unreachable creates two writers.

1. Confirm the old authority is actually down from more than one vantage
   point (its host, a second network, a second operator).
2. Establish the old authority **cannot come back as a writer**: stop the
   process and disable its service, and revoke its access to the signing
   key. If it might still be running, do not continue.
3. Announce the incident to every operator who mirrors this network, out of
   band, before doing anything else.

## Step 1: freeze and preserve the mirror

On the mirror you intend to trust, stop anything that would change its
copy while you inspect it (the mirror-watcher backfills from the
authority, which is down, so this is mostly a safeguard), then take a
database backup. This backup is the only copy of the history if the
authority's data is lost.

## Step 2: verify convergence before trusting the mirror

```
avalon verify-mirror-convergence <network_id> [--shard-id <id>] [--source <url>]
```

(`--source` limits the check to entries mirrored from one specific peer.)

This works offline, from the mirror's own stored data, so it does not need
the dead authority. It recomputes the Merkle root over every mirrored entry
and requires that root to equal an observed, signature-verified Signed Tree
Head at the furthest size any peer attested to, with an unbroken hash chain
and no open equivocation. It exits `0` only when converged.

| Verdict | Meaning | What to do |
|---|---|---|
| `CONVERGED` | The mirror holds the complete, verified history | Continue |
| `BEHIND` | An STH attests to more entries than the mirror holds; the tail is missing | Run the check on the other mirrors. The most complete converged mirror is the one to promote. If none has the tail, entries after the mirror's last size are lost. |
| `ROOT_MISMATCH` | No observed STH has the recomputed root | Do not promote. Investigate as in [`equivocation-response.md`](equivocation-response.md). |
| `CHAIN_BROKEN` | A mirrored entry does not link to its predecessor | Do not promote this copy. |
| `BLOCKED` | An unresolved equivocation exists | Resolve it first ([`equivocation-response.md`](equivocation-response.md)). |
| `NO_OBSERVED_STH`, `NOTHING_MIRRORED` | Nothing to verify against | This node cannot be promoted. |

If several mirrors exist, run the check on each and promote the one with the
largest converged `tree_size`. Compare the printed root across mirrors; they
must agree.

## Step 3: decide which key signs from now on

The new authority signs Signed Tree Heads with `AVALON_SETTLEMENT_SIGNING_KEY`.

- **The old key is recoverable and trusted** (for example from a backup, and
  the outage was not a compromise): reuse it with the same
  `AVALON_SETTLEMENT_SIGNING_KEY_ID`. Nothing that verifies STHs changes, so
  step 6 is unnecessary. This is by far the least disruptive path.
- **The old key is lost or suspect**: generate a new key and a new key id, as
  in [`key-rotation.md`](key-rotation.md). Every mirror and client pinned to
  the old verify key will reject the new STHs until it is updated, so this
  is a coordinated rollout, not a local change. Treat it as the emergency
  rotation described there.

## Step 4: bring up the new authority

1. **Seed a fresh database from the converged mirror.** The target must be a
   separate, already-migrated database (`make migrate` against it) whose
   `ledger_entries`, `ledger_batches` and `signed_tree_heads` are empty, and
   whose genesis, if it has one, equals the network id. A node that already
   authors a different shard cannot be seeded in place: give the promoted
   shard its own database. On the mirror, with `DATABASE_URL` pointing at the
   mirror's database:

   ```
   avalon promote-mirror <network_id> [--shard-id <id>] [--source <url>] \
       --target-database-url <url> [--dry-run]
   ```

   Run it with `--dry-run` first: every check runs and the plan is printed
   without writing. The command refuses unless the mirror is `CONVERGED`
   (it prints the same verdict as step 2), then, in one target transaction,
   re-verifies every hash link and every entry hash whose payload is still
   stored, copies each entry with its original `seq`, rebuilds the batch
   records, carries over every observed Signed Tree Head whose root matches
   the recomputed tree at that size (a root that does not match is never
   copied), and moves the sequence past the highest imported `seq`. It then
   reopens the target and checks the chain, the entry count and the root
   against the converged values. Entries whose payload the mirror had pruned
   are carried without content.

   What promotion does not do: it does not carry sessions or login
   credentials (people log in again); it loses any entry the mirror never
   saw, which is why step 2 must report `CONVERGED` from the most complete
   mirror; and the carried Signed Tree Heads stay signed by the old key,
   whatever key signs from now on.
2. On the promoted host, set `AVALON_NETWORK_ID` to the network's id (it
   must equal the ledger's genesis network id; a mismatch is fatal at boot),
   `AVALON_SETTLEMENT_SIGNING_KEY`, `AVALON_SETTLEMENT_SIGNING_KEY_ID`, and
   `AVALON_OWN_SHARD_ID` to the shard being taken over (`core` by default).
   The signing key must be the one registered or pinned for that shard: for
   `core`, the key pinned in `docs/trusted-networks.json`; for a named shard,
   a registered `shard_settlement` key (see
   [`../for-hosters/choosing-your-shard.md`](../for-hosters/choosing-your-shard.md)).
   If the lost authority's key is not being reused, generate a new key on the
   promoted host and register its public half with `avalon add-shard-key`
   before starting the node; the integrator's other shard keys stay valid, so
   also revoke the lost key if it may be compromised.
3. Unset `AVALON_SETTLEMENT_REMOTE_URL(S)` for that shard, so this node
   commits locally rather than forwarding to the dead authority, and remove
   the dead authority from `AVALON_MIRROR_PEERS`.
4. Set `DATABASE_URL` to the seeded database (or the restored one) and start
   `avalon-server`.
5. Rebuild derived state from the ledger:

   ```
   avalon rebuild-index
   ```

   The rebuild recreates each identity's registry row from its
   `identity.created` event first, so it works on a database that never held
   those identities. Sessions and login credentials are server-local and are
   not in the log, so people log in again; everything the protocol promises
   durably is rebuilt from history
   ([`../architecture/disaster-recovery.md`](../architecture/disaster-recovery.md)).
6. Run `avalon inspect-ledger` and confirm `chain intact` and an entry count
   equal to the converged `tree_size` from step 2. It reports the carried
   tree heads as failing signature verification when the configured verify key
   is not the lost authority's key; that is expected for heads the old key
   signed, and the Merkle recompute line is the check that matters for them.
7. Check `GET /ledger/sth/latest`: `network_id` and `tree_size` must match,
   and the root at that size must equal the converged root you recorded.

## Step 5: repoint the rest of the network

- Every other node that forwarded this shard's writes: set
  `AVALON_SETTLEMENT_REMOTE_URL` (or the `core=` entry of
  `AVALON_SETTLEMENT_REMOTE_URLS`) to the new authority and restart.
- Every mirror: set `AVALON_MIRROR_PEERS` to the new authority, and, if the
  key changed, `AVALON_SETTLEMENT_VERIFY_KEY`, following the ordering in
  [`key-rotation.md`](key-rotation.md) (a mirror holds exactly one verify
  key, so updating it early or late stalls that mirror).
- Confirm each mirror resumes advancing and that
  `avalon list-equivocations <network_id>` shows no new finding.

## Step 6: update the trust anchors (only if the key or URL changed)

Clients pin the network through `docs/trusted-networks.json`. Open a pull
request that updates that network's entry (`verify_key`, `signing_key_id`,
`server_url`, `seed_nodes`) to the promoted node; that reviewed change is the
governance control. SDK builds and the Hub bundle the file at build time, so
already-shipped builds keep the old key until they are rebuilt, and show the
network as a key mismatch in the meantime. Until they are updated, the
runbook's announcement (step 0) is how users learn why. See
[`../architecture/network-trust-anchors.md`](../architecture/network-trust-anchors.md).

## After the incident

Keep the old authority off the network. If it is ever restored, it must not
resume writing under the same shard: bring it back only as a mirror of the
new authority, on a fresh database, or retire it.
