# Witness Cosigning

Recorded decisions: [#945](https://github.com/avalon-initiative/avalon-protocol/issues/945)
(core trust is witness cosigning, not a validator committee) and
[#929](https://github.com/avalon-initiative/avalon-protocol/issues/929) (shard
identity is self-certifying). This document is the design-spike output for
[#934](https://github.com/avalon-initiative/avalon-protocol/issues/934):
concrete parameters, reasoning, and a prototype proving the two properties
the whole scheme depends on. Epic: [#942](https://github.com/avalon-initiative/avalon-protocol/issues/942).

**The goal, stated once so every parameter below can be checked against it:**
no node, key, or operator is required for the network to keep operating or
for a client to keep verifying it — including the original node. A network of
one behaves under the same rule as a network of ten thousand; nothing here is
a special case for small or large N.

## Why this replaces the single pinned key

Today (`sth.rs`, `network-trust-anchors.md`) one Ed25519 key — the settlement
operator's — signs every Signed Tree Head, and every client pins that one
key. That key, and whoever holds it, is a single point of both trust and
availability: if it's lost, compromised, or its operator disappears, nothing
can be verified or extended in its name again. Witness cosigning replaces
"trust one key" with "trust that a majority of a bounded, self-filling known
list of witnesses independently checked this head and found no conflicting
one" — the same head-verification job, done by whoever happens to be
running a node, with no membership vote, no token, and no committee.

## Cosignature format

A witness cosignature is a **second, independent signature** over the same
`(tree_size, root_hash, network_id, created_at)` an author's own STH already
signs (`sth::signing_message`) — the author doesn't change how it signs;
witnesses add a layer on top. Prototype: `crates/protocol/src/witness.rs`,
`WitnessCosignature` / `witness_signing_message` (domain tag
`avalon-witness-cosign-v1`, so a cosignature can never be confused with an
author's own STH signature even though the fields overlap).

Fields, and why each is there: `tree_size`/`root_hash`/`network_id`/
`author_created_at` bind the cosignature to one specific STH. `witness_key_id`
identifies which witness signed (the known list is keyed by this, never by
network address — an address can be spoofed or change; a key can't).
`observed_at` is the witness's own timestamp, separate from the author's —
what freshness checks use (below). An old cosignature for a since-superseded
head stays valid forever (the head it attests to was real); a witness that
hasn't produced a *fresh* one lately is a stale slot, which is a different
concern from "is this cosignature real."

**What a witness checks before cosigning** (this is the actual security
work; the signature is just how the result gets published):

1. The STH's author signature verifies against the shard's currently-
   authorized key (self-certifying per #929/#931, or resolved via the core
   registry for a named shard — `network-trust-anchors.md`'s per-shard
   section already describes this resolution).
2. A consistency proof from the last `(tree_size, root_hash)` this witness
   itself last cosigned for that network/shard to the new one — proves an
   append-only extension, not a rewrite.
3. This witness has not itself already cosigned a *different* root at this
   exact `tree_size` (a local, trivial equivocation guard on its own past
   behavior).

Only then does it produce a `WitnessCosignature`. A verifier never needs to
re-derive this — a cosignature's existence *is* the witness's claim to have
done it, same as any other signed statement in this protocol.

## Default parameters

| Parameter | Default | |
|---|---|---|
| Known-list size (Y) | 10, hard cap | Bounded verification cost per node/client regardless of network size; large enough for real diversity, small enough to gossip and check cheaply. |
| Cosigning threshold (X) | `majority_threshold(Y_actual) = Y_actual / 2 + 1` | Not a fixed 6 — recomputed against whatever the list's *actual* current size is. At Y_actual=1 (a lone node with no peers yet) X=1: self-attestation, the same rule degenerating correctly to today's single-signer case rather than a special-cased bootstrap mode. At the Y=10 cap, X=6. |
| Freshness window | 10 minutes, configurable | Slot-health/refill trigger only (see below) — never head validity. |
| Anchor slots | 2 of the 10 | Reserved for bundled anchors (see below); never filled by ordinary refill. |
| Diversity cap | 2 slots per prefix | Applies to every slot, anchors included. |
| Head-gossip cap | ≤5 head summaries per exchange | Matches the existing per-exchange gossip discipline (#882/#948) — small, bounded payload, not full cosignature bytes on every exchange. |

**Why `X = Y_actual/2 + 1` and not a fixed number:** this is the entire
fork-detection guarantee, and it only holds if X is always strictly more than
half of whatever list size is actually in play. `witness.rs`'s
`majority_threshold_always_exceeds_half_the_list` test checks
`2 * threshold > list_size` for every size from 1 to 10,000 directly; two
tests below it double-check the same property by literally enumerating (for
small lists) or randomly sampling (for lists up to 100) subset pairs and
confirming every pair of majority-sized subsets shares a member. **This is
what makes an equivocation provable, not just unlikely:** two different heads
at the same `tree_size`, each independently majority-cosigned by
*possibly-different* observers, can only both exist if some single witness
cosigned both — which is either impossible (no fork) or that witness broke
rule 3 above, which is now provable from the two cosignature sets themselves.

## The known list: anchors, diversity, refill

Prototype: `crates/protocol/src/known_list.rs`, `KnownList`. Every node and
every SDK client keeps its **own** bounded list — there is no single global
list anyone maintains, which is itself part of what makes eclipse resistance
possible (an attacker facing one victim's list gains nothing against anyone
else's).

- **2 anchor slots**, seeded from `docs/trusted-networks.json`'s
  `seed_nodes` (already operator-diverse, PR-reviewed, long-lived by
  convention). These exist purely so a *freshly-booted* node or SDK isn't
  computing its first-ever majority entirely from whoever it happened to
  discover first — not because an anchor's cosignature counts for more (it
  doesn't; it's 2 of Y in the same count as everyone else) and not because
  anchors are permanent (`KnownList::remove` can drop one, same as any
  slot — an anchor going offline is handled by ordinary refill once it's
  gone, not treated as a special failure).
- **8 self-filling slots**, populated from discovery, capped at 2 per
  diversity prefix (an IPv4 /24, IPv6 prefix, or operator id) — including
  the anchor slots in that same cap, so one operator can't buy outsized
  influence by also running the bundled anchors.
  `a_flood_of_same_prefix_candidates_cannot_exceed_the_diversity_cap` proves
  this holds under an arbitrarily large flood of same-prefix candidates.
- **Persisted across restarts** — a node doesn't rebuild its list from
  scratch on every boot, which would hand a well-timed attacker a fresh
  shot at eclipsing it every time it restarts.
- **Refill after loss**: when a slot's occupant goes stale (see freshness,
  below) or is otherwise removed, the freed slot is refilled from fresh
  discovery under the same diversity rule.
  `the_list_refills_to_capacity_after_witness_loss_and_stays_diverse` runs
  this end to end — losing 3 of 10, refilling to 10 again, checking
  `majority_threshold` recomputes correctly at every size in between (4 at
  7 members, back to 6 at 10) with no special-cased code path for "the list
  just changed size."
- Not built into this prototype, left to #946: tenure-weighted preference
  among discovery candidates, and a probation period before a brand-new
  candidate counts toward majority (both raise the cost of a burst Sybil
  flood timed right before an attack, without changing the core algorithm
  above).

## Freshness window

10 minutes by default. A witness whose most recent `observed_at` for a
network's current head is older than the window is a **stale slot** — eligible
for replacement by ordinary refill — not a signal that anything it signed in
the past becomes invalid. Old cosigned heads remain valid forever (consistency
proofs only ever extend forward); freshness is entirely about "is this slot's
occupant still alive and worth a seat," the input to refill, never an input
to verifying a past head.

## Head gossip

Nodes gossip a bounded (≤5 per exchange) list of `(network_id/shard_id,
tree_size, root_hash, cosignature_count)` summaries alongside the existing
peer-gossip exchange (`nodes.rs::merge_gossip`) — not full cosignature bytes
on every exchange, matching the size discipline #896 already established for
node-coordination routes. A node or client that wants the full cosignature
set for a summary it's seen fetches it directly (e.g.
`GET /ledger/sth/latest?witnesses=1`, left to the implementation ticket).

**Fork detection is a side effect of this gossip, not a separate mechanism.**
Any node or client that ever observes two different cosigned heads at the
same `tree_size` for the same network/shard has direct proof of an
equivocation (by the majority-intersection guarantee above, this can only
happen if a real witness double-signed). That's treated as a loud,
logged, "stop and don't trust either head" event — never silently
auto-resolved.

## Network sizes from one node upward

There is exactly one rule — `X = majority_threshold(current known-list size)`
— and it is evaluated identically regardless of how many nodes exist. At
Y_actual=1 a node is its own sole witness (X=1, self-attestation, matching
today's behavior exactly). As peers are discovered the list grows toward the
Y=10 cap and X is recomputed each time membership changes. Nothing in this
design distinguishes "the original node" from any other — the original
node's departure just looks like any other witness leaving a list, handled by
ordinary refill.

## Recovery when an entire known list is captured or lost

This is a rare, human-noticed event, not something automated recovery should
paper over. If a node or client's entire known list goes unreachable past the
freshness window, or a confirmed equivocation is detected among what looked
like a healthy majority, it falls back to bootstrapping a fresh list from
whatever anchors and seed nodes the *currently installed* software ships with
— exactly like a first boot. Recovering to a *different* anchor set (because
the old one is now known-bad) is an ordinary, PR-reviewed update to
`trusted-networks.json` shipped in a normal release — the same governance
`network-trust-anchors.md` already describes ("changing this file goes
through normal repo review"), not a new mechanism. This deliberately does not
auto-trust "whoever answers first" after a loss.

## Prototype: what it proves and what it doesn't

`crates/protocol/src/witness.rs` and `crates/protocol/src/known_list.rs` are
real, tested, pure-logic code — not pseudocode — but they are the *design
spike's* prototype, not the production implementation:

- **Proves**: the signature format round-trips and rejects tampering; the
  majority-threshold rule guarantees any two majority-sized subsets of a
  known list intersect, at every list size (both by direct arithmetic check
  and by brute-force/random subset enumeration); a known list with anchor
  reservation and a diversity cap resists a same-prefix flood and correctly
  refills to capacity after losing members, staying diverse throughout.
- **Does not include**: server wiring, storage, real discovery integration,
  tenure-weighted selection, probation periods, or the gossip transport
  itself — those are #931 (self-certifying ids), #932 (cosigned tree head
  format/verification wired into the server), #936 (naming layer), #938
  (mirror sync with cosigned heads), #939 (migration from single-key
  networks), #944 (per-identity chains), #946 (production known-list
  management), #947 (head gossip).

## Today in the repo

`crates/protocol/src/witness.rs` and `crates/protocol/src/known_list.rs`
exist and are tested (see above); nothing else in this document is wired into
`crates/server` yet — the network still runs on the single pinned
`AVALON_SETTLEMENT_VERIFY_KEY` model `sth.rs`/`network-trust-anchors.md`
describe until the implementation tickets land.

## Decisions and tickets

[#945](https://github.com/avalon-initiative/avalon-protocol/issues/945),
[#929](https://github.com/avalon-initiative/avalon-protocol/issues/929),
[#934](https://github.com/avalon-initiative/avalon-protocol/issues/934) (this
document), epic
[#942](https://github.com/avalon-initiative/avalon-protocol/issues/942) and
its sub-issues.
