# Equivocation response

What to do when this node's mirror-watcher reports equivocation — two
different signed tree heads (STHs) for the same network and `tree_size`,
from the same operator key. Scope decided in issue #300, implemented in
#316: an investigation playbook plus a mirror-recovery mechanism. Key
rotation itself is covered in [`key-rotation.md`](key-rotation.md); real
alerting/paging for equivocation detection specifically is **not** covered
here yet — tracked separately in #315.

## Background

`crates/chain/src/mirror.rs`'s `detect_equivocation` and
`crates/server/src/mirror_watcher.rs` build *detection* only (#299): a
mirror that observes two disagreeing root hashes for the same
`network_id`/`tree_size` records it durably in `equivocation_findings` and
logs it loudly. Once detected, `mirror_watcher::backfill_network` refuses
to extend that network's mirrored history at all until every finding for
it is resolved — this is the "stop trusting the current head" behavior;
nothing else needs to be done to get that protection, it's already
automatic.

What detection cannot tell you is *which* of the two root hashes was
legitimate — both are validly signed by the same key. That's what this
playbook is for.

## Step 1: check whether you have an open finding

```
avalon list-equivocations [network_id]
```

Omit `network_id` to default to this node's own genesis network. Every
finding this node has ever recorded is listed, marked resolved or
unresolved. Only unresolved findings block the mirror-watcher.

## Step 2: investigate — rule out a benign cause first

A single operator honestly producing two different signed trees at the
same `tree_size` is not automatically an attack. This exact settlement
code has already had real bugs that could plausibly produce this shape of
symptom — check these first, in order, before assuming key compromise:

1. **Concurrent-commit race.** Did two batch-commit attempts race against
   the same `tree_size`, with one committing successfully and the other
   observed before it was fully rolled back or retried? Check the
   authority node's logs/`ledger_batches` around the disputed `tree_size`
   for overlapping commit attempts.
2. **`tree_size`/`seq` confusion.** Confirm the two disagreeing STHs are
   genuinely claiming the *same* `tree_size` (leaf count) and not, e.g.,
   one side conflating `tree_size` with the highest raw `seq` value across
   a gap (Postgres `GENERATED ALWAYS AS IDENTITY` burns `seq` values on a
   rolled-back commit — see `avalon inspect-ledger`'s own doc comment on
   this). Re-run `avalon inspect-ledger-full` against the authority node
   directly and compare its live Merkle recompute at that `tree_size`
   against both disputed root hashes.
3. **A genuinely stale/reverted observation.** Is `source_a` or `source_b`
   a mirror that was serving a snapshot from before a legitimate
   corrective action (e.g., a restored backup) rather than two live,
   concurrent signed statements? Check `observed_at` on both rows in
   `observed_sths`.
4. **If none of the above explain it**: treat it as a real compromise
   candidate. Do not attempt automatic resolution — this is a human
   trust judgment, matching Certificate Transparency's own precedent (see
   `docs/architecture/settlement.md`). Escalate to whoever holds the
   operator/validator signing key for this network, then follow the
   emergency path in [`key-rotation.md`](key-rotation.md).

## Step 3: resolve the finding

Once you've determined which root hash was legitimate:

```
avalon resolve-equivocation <network_id> <tree_size> <legitimate_root_hash>
```

This marks every unresolved finding at that `network_id`/`tree_size`
resolved, unblocking the mirror-watcher's backfill gate for this network.

## Step 4: recover this node's own mirrored state

If this node might have already mirrored content verified against the
*other* (losing) branch's tree before detection caught it — plausible if a
previously-partitioned or slow-to-report peer surfaces an old, conflicting
STH after this node already advanced past that `tree_size` on a different
peer's say-so — re-run step 3 with the recovery flag:

```
avalon resolve-equivocation <network_id> <tree_size> <legitimate_root_hash> --discard-mirrored
```

This additionally deletes every `mirrored_entries` row this node verified
at or beyond `tree_size` for that network. It is deliberately coarse: it
drops content from *both* branches at or past that point, not just the
losing one, because there is no local way to tell after the fact which
already-mirrored row came from which branch. The mirror-watcher's normal
multi-peer backfill (#299) re-fetches and re-verifies everything dropped
this way from the now-resolved-legitimate branch on its next tick — this
is re-verification, not permanent data loss of anything the network
itself considers canonical.

If this node never actually advanced past `tree_size` (the common case,
since the equivocation gate stops backfill the same tick detection fires),
`--discard-mirrored` is a safe no-op — there's nothing to discard.

## What this does not cover

- **Alerting.** Nothing pages a human today; `avalon list-equivocations`
  and the mirror-watcher's own log output are the only way to notice a
  finding right now. Tracked in #315, depends on #265 (structured
  logging).
- **Automated rotation.** [`key-rotation.md`](key-rotation.md) is a written
  manual procedure, not automation — nothing here rotates a key for you.
- **Automatic resolution.** Deliberately never built — see "Trust model:
  why no witness quorum" in issue #301 and `docs/architecture/settlement.md`
  for why this stays a human decision.
