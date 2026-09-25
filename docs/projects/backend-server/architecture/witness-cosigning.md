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

**The author counts as the witness that holds its key.** A head's own author
signature already proves that key vouches for it, so `verify_cosigned_tree_head`
credits a known witness whose key is the author's without a separate
cosignature (the author signature is still verified first, and a head with a bad
author signature earns no credit). Without this, a known list that contains the
shard's own author, which happens whenever the author also announces a witness key
and holds a slot, could never reach a majority for that shard, since an author never
cosigns its own log. The rule is part of the cosigned-head conformance vectors.

**This holds for one known list only.** The intersection guarantee is about
majorities of a single list. Two nodes with different lists (or two heads each
cosigned by a majority of a different list) can produce two majority-cosigned
heads with no shared witness, and then the pair proves nothing about any
witness. What still proves misbehavior is the author's own signature: two
heads at one `tree_size` with different roots, both signed by the same
resolvable author key, show the author signed two roots, with no cosignature
needed. See "Author-level evidence" below.

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
equivocation of the author, and of a witness too when the two cosignature sets
share one (the majority-intersection guarantee, valid within one known list).
That's treated as a loud,
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
- **Does not include** what the production code added afterwards: server
  wiring, storage, real discovery, tenure-weighted selection, probation, the
  gossip transport and the cosigning decision. See "Today in the repo" for
  what exists now and "Not done yet" for what does not.

## Today in the repo

`crates/protocol/src/witness.rs` and `crates/protocol/src/known_list.rs`
exist and are tested (see above). #932 added
`crates/protocol/src/cosigned_sth.rs`: `CosignedTreeHead` (one author
`sth::SignedTreeHead` plus its `Vec<witness::WitnessCosignature>`),
`verify_cosigned_tree_head` (author signature + majority-of-known-list
cosignature check, degenerating to exactly `sth::verify_tree_head` at a
known-list size of 0 or 1), and `find_equivocating_witnesses` (the
concrete equivocation proof for two conflicting majority-cosigned heads).
`crates/chain::PostgresSettlementProvider` gained storage for cosignatures
(`witness_cosignatures` table, migration `0073_witness_cosignatures`,
shard-scoped by migration `0075_witness_cosignatures_shard_scoping`) and a
read path (`cosigned_tree_head_at`) that assembles a `CosignedTreeHead`
from stored state for a caller to verify. `conformance/vectors/
witness-cosigned-tree-head.json` covers accepted/below-threshold/
unknown-witness/stale/conflicting-heads, asserted from
`crates/protocol/tests/conformance.rs`. #946 landed production known-list
management (`crates/server/src/known_list.rs`): persistence across
restarts, real `PeerTable`-sourced discovery, probation, tenure-weighted
refill — held in `AppState::known_list`, read live by every verification
site below.

#947 added `avalon-server`'s head-summary gossip: `crate::nodes::HeadSummary`/
`HeadGossipTracker` (its own bounded structure, never folded into
`PeerTable`/`ShardRegistry`) rides the same `POST /nodes/announce` exchange
as peer/shard gossip, capped at `MAX_HEAD_SUMMARIES_PER_EXCHANGE` (5) per
exchange, validated (shape/hex checks reusing `PeerAdmission::shard_id_ok`)
before ever being relayed. `GET /ledger/sth/latest`/`/ledger/sth/{tree_size}`
now take `?witnesses=1` (`crate::settlement::WitnessesQuery`) and return
stored cosignatures alongside the STH — the fetch-on-demand half. A
conflicting pair of gossiped summaries (same shard/tree_size, different
root) is confirmed by `crate::equivocation::confirm_and_record`: fetches
full cosignature detail from both reporting peers, builds its known list
directly from the hex-encoded witness keys present in the two heads (so the
resulting proof is independently checkable from signatures alone, not
dependent on any node's own trusted list), and calls
`find_equivocating_witnesses`. A confirmed equivocation is recorded via
`avalon_chain::mirror::record_witness_equivocation_evidence` — the same
table and function #938 (landed the same day) uses for equivocations it
finds directly during mirror sync, reconciled into one write path rather
than two competing `equivocation_evidence` schemas — and marks the shard in
`HeadGossipTracker::is_equivocating`.

**Author-level evidence.** Confirmation does not require cosignatures. When a
gossiped conflict is confirmed, each side is also topped up with cosignatures
fetched from the confirmed known-list witnesses
(`crate::cosign_gather::gather_witness_cosignatures`), so a bare author that serves
none can still be shown to carry a majority. Then: if the two heads share a
verifying witness, the row is stored as `witness` evidence naming it; otherwise,
if both author signatures verify under an author key this node resolves (the
pinned core key or the shard's registered `shard_settlement` keys) and the
roots differ at the same network and `tree_size`, the row is stored as
`author` evidence with an empty witness list. `equivocation_evidence` gained an
`evidence_kind` column (migration `0079_equivocation_evidence_kind`; existing
rows read as `witness`; a `witness` row must still name at least one witness).
Either kind marks the shard equivocating, so `witness_cosign` refuses to cosign
it. `crates/server/examples/witness_evidence.rs` reads the rows back.

What remains unprovable: two majority-cosigned heads with different lists and no
shared witness say nothing about any witness (only the author's double-signing
is proven, and only when this node can resolve the author key); a node that
cannot resolve the author key records nothing; a fork shown to one observer
and never gossiped is not detected by anyone else; and evidence proves the
author signed two roots, not which one is the honest history.

#963 added the cosigning decision itself (`crates/server/src/witness_cosign.rs`),
called from `mirror_watcher::record_verified_head` right after a head passes
author-signature and majority-cosignature verification. It refuses first if
`HeadGossipTracker::is_equivocating` is set for the shard, then compares the
head against this node's own last-cosigned checkpoint for that network/shard
(`witness_checkpoints`, migration `0077_witness_checkpoints`,
`avalon_chain::mirror::witness_checkpoint_for`/`record_witness_checkpoint`):
no checkpoint means cosign unconditionally (bootstrap); the same size and
root is a no-op; the same size with a different root is refused
(no-double-cosign); a smaller size is refused as stale; a larger size needs an
RFC 6962 consistency proof, fetched from the peer that served the head and
verified locally with `avalon_chain::merkle::verify_consistency_proof`
against the checkpoint root and the head root. The checkpoint advances
before the cosignature is signed and stored, so a crash between the two
withholds a cosignature (repaired the next time the head is seen) and can
never leave a cosignature without a checkpoint. The witness key is
`AVALON_WITNESS_SIGNING_KEY`, falling back to `AVALON_SETTLEMENT_SIGNING_KEY`
(the cosignature's own domain tag keeps one key safe across both uses); its
id defaults to the hex verifying key. `AVALON_WITNESS_COSIGNING_ENABLED=false`
opts a node out; a node with no usable key never cosigns.

`#947`'s equivocation confirmation derives its known list straight from the
two conflicting heads' own cosignatures instead of any node's production
known list — its proof stands on its own.

#938 wired cosigned verification into `avalon-server` itself:
`crates/server/src/cosign_verify.rs` bridges `KnownListHandle`'s confirmed
witness identities into `verify_cosigned_tree_head`'s
`(witness_key_id, VerifyingKey)` shape and holds the shared
`WitnessCosignatureDto` wire format. `GET /ledger/sth/latest`/
`GET /ledger/sth/{tree_size}` now serve every stored cosignature for that
head alongside it (`SignedTreeHeadResponse::cosignatures`). Every mirror/
cross-shard verification call site that used to check a bare author
signature now checks by majority cosignature instead —
`mirror_watcher::fetch_and_verify_sth`/`discover_and_verify_shard_peers`,
`cross_shard::fetch_and_compute`, `cross_shard_fetch::fetch_verified_sth`
— replacing the old check outright (not running alongside it), since
`verify_cosigned_tree_head`'s own `known_list.len() <= 1` degenerate case
already reproduces it exactly. The known list is read fresh at the top of
every mirror-watcher poll tick, so an admitted or dropped witness takes
effect on the very next tick, no restart. A newly-observed head's valid,
known-list-recognized cosignatures are durably stored
(`store_valid_cosignatures`) so this node can re-serve them to further
mirrors. When two heads accepted this tick for the same
network/shard/tree_size disagree and the known list has more than one
witness, `find_equivocating_witnesses` runs and any non-empty result is
durably recorded in a new `equivocation_evidence` table (migration
`0076_equivocation_evidence`, `avalon_chain::mirror::
record_witness_equivocation_evidence`/`witness_equivocation_evidence_for`)
— both full heads' signed fields and both cosignature sets, so the proof
is reconstructable and re-verifiable from the stored row alone, by anyone,
independent of this node. This is separate from #299's original
source-based `equivocation_findings` table (any two peers disagreeing,
cosigning or not), which is unchanged and still the broader net.

**Known-list slots are keyed by the witness's cosigning key (#967).** A node
that cosigns advertises `witness: {key_id, announced_at, proof}` in
`POST /nodes/announce` requests and responses and in the peer entries it
gossips. `key_id` is the hex verifying key; `proof` is that key's signature
over `(base_url, key_id, announced_at)` under the domain tag
`avalon-witness-announce-v1` (`avalon_protocol::witness::
sign_witness_announce`/`verify_witness_announce`), accepted only within an
hour of the verifier's clock. A forged, mismatched or stale proof is dropped
and the peer is still admitted to the peer table, just without a key. Only
peers with a fresh direct proven key become known-list candidates, keyed by that key,
so `cosign_verify::known_list_verifying_keys` yields real
`(witness_key_id, VerifyingKey)` pairs and a node's own cosignatures count
toward a majority. Bundled anchors get their key from their own announce
under their seed URL (an anchor with no proven key yet is not a candidate).
A node whose `AVALON_WITNESS_SIGNING_KEY_ID` override is not the hex
verifying key advertises nothing. A persisted `known_list.json` from before
this scheme carries no format version and is discarded on load. Cosignatures
reach a node's `witness_cosignatures` table from its own cosigning decision
as well as from `store_valid_cosignatures` while mirroring; #947's gossip
carries bounded *summaries*, not a proactive push of new cosignatures. The
pinned `AVALON_SETTLEMENT_VERIFY_KEY` remains the log's own signer and what
released clients check; cosigning is additive and needs no reset. The
procedure for moving a running network onto it and back is
[`../for-maintainers/witness-policy-rollout.md`](../for-maintainers/witness-policy-rollout.md).
The proof alone shows only that the key holder bound the key to that URL,
not that the URL's operator agrees, so it is not enough to become a
candidate: an attacker could otherwise gossip an innocent host's URL bound to
its own key and inherit that host's diversity prefix. A stored advert
therefore carries a non-serialized `direct` flag, set only when it was
verified from the announce response received from that exact `base_url`
(the URL's own endpoint vouches for the key). Adverts from inbound announces,
gossip and the unverified pool are `direct = false`, are never candidates,
never displace a direct advert, and a keyless entry never erases one.
A node obtains a direct advert by announcing to the peer: every tick the
announce worker also contacts up to 5 table peers that hold only a non-direct
advert (round-robin by base URL, skipping active peers, which are contacted
anyway), so a node with no outbound bootstrap peers, such as a seed that only
receives announces, still vouches its inbound-only peers. These contacts
pass the outbound address policy, use the announce timeout, take only the
advert from the response, and never touch the active set or neighbors table.

**Cosigning does not wait on majority, and mirrors gather cosignatures (#970).**
A head reaches `witness_cosign::decide_and_cosign` as soon as its author
signature verifies, with the consistency and no-double-cosign guards
unchanged; majority is only what decides whether this node then trusts and
stores the head. An author node serves no cosignatures of its own, so a
mirror with a known list of more than one witness gathers them itself
(`crates/server/src/cosign_gather.rs`): the cosignatures attached to the
source's response plus, for each confirmed known-list witness, a
`GET {witness_base_url}/ledger/sth/{tree_size}?shard_id=<shard>&witnesses=1`.
A witness's base URL is the peer-table entry whose *direct* witness advert
carries that witness key; witnesses with no such entry are skipped. Fetches
go through the outbound address policy, time out after 5 seconds, run at
most 8 at a time and read at most 64 KiB; a response is used only if its
root, size, network and creation time equal the head's, and each
cosignature must still verify against the known-list key. If majority is
reached the head is stored and trusted as before, with the gathered
cosignatures stored for re-serving; otherwise it is held (logged once at
info) and retried on the next tick. A known list of 0 or 1 keeps the plain
author-signature path. The same helper backs `cross_shard::fetch_and_compute`
and `cross_shard_fetch::fetch_verified_sth`, which verify a remote shard's
head from its author and had the same dependency. A mirror that has cosigned
a head but not yet backfilled it serves that observed head with its
cosignature from `GET /ledger/sth/{tree_size}` (only when the head's root
matches a stored cosignature), so witnesses can serve each other before any
of them has backfilled.

## Not done yet

- **Clients do not verify cosignatures.** The SDKs and the Hub check the author
  signature against the pinned key. Cosigned verification in all three SDKs, with the
  conformance vectors, is avalon-initiative/avalon-sdks#37 and ships in the
  coordinated SDK release.
- **No witness policy in the trust-anchor entry.** `docs/trusted-networks.json` is
  unchanged: same pinned key, same seed nodes. Whether an entry should carry witness
  keys or a client policy is decided together with the SDK work.
- **The live drill has not been run on the dev fleet with cosigning on**, and the
  `fork` scenario does not yet exercise two disjoint witness groups. Tracked in #966.
- **Layer-1 per-identity chains are not wired into the server.** The pure conflict
  rule and types exist (`identity_chain.rs`); chain state, emission sites, the indexer
  and the freeze on a forked identity are #961, with conformance vectors.
- **Resistance, not proof.** An attacker who controls most of a victim's known
  witnesses can still mislead that victim. The diversity cap, probation, anchors and
  the vouch requirement raise the cost; they do not remove it.

The scenario suite that exercises growth, loss of the original node, witness loss, eclipse and fork attempts against real processes, plus the live-fleet drill procedure, is in [`../for-maintainers/witness-drill.md`](../for-maintainers/witness-drill.md) (`scripts/witness-drill.sh`).

## Decisions and tickets

[#945](https://github.com/avalon-initiative/avalon-protocol/issues/945),
[#929](https://github.com/avalon-initiative/avalon-protocol/issues/929),
[#934](https://github.com/avalon-initiative/avalon-protocol/issues/934) (this
document), epic
[#942](https://github.com/avalon-initiative/avalon-protocol/issues/942) and
its sub-issues.
