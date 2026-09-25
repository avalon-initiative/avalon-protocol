# Network Trust Anchors

**`network_id` alone is never sufficient to trust a server.** It is a plain
string with zero cryptographic authority — anyone can stand up their own
`avalon-server`, set `AVALON_NETWORK_ID=avalon-mainnet-1` (the exact string a
real deployment uses), and serve fabricated history under that name. The only
thing that actually distinguishes the real network from an impostor is
whether its Signed Tree Heads verify against the *specific* Ed25519 public
key belonging to the real settlement operator. This document covers where
that key is published and how a client pins it — the same role a browser's
pinned CA root list or SSH's `known_hosts` plays.

## The trust-anchor list

[`../trusted-networks.json`](../../../trusted-networks.json) is the canonical,
versioned, publicly-published trust-anchor list. Each entry:

| Field | Meaning |
|---|---|
| `label` | Human-readable name for the network. |
| `network_id` | The exact string a server sets `AVALON_NETWORK_ID` to and bakes into its ledger's genesis. |
| `verify_key` | Hex-encoded Ed25519 public key — the public half of that network's settlement operator signing key (`AVALON_SETTLEMENT_VERIFY_KEY`, see `crates/protocol/src/sth.rs` and `.env.example`). |
| `signing_key_id` | Which key generation this is, matching `SignedTreeHead.signing_key_id` — informational; a rotated key gets a new entry (or a documented rotation), not a silent overwrite of this one. |
| `environment` | Which tier this deployment is: `local-dev` (no real deployment — a freely-generated key checked in only to exercise the mechanism end to end), `dev` (a real, non-production, single-node deployment — infra on one machine), `int` (a real, non-production, 1-5 node interconnected test bed used to verify changes actually integrate across nodes before they reach mainnet), or `prod` (a real mainnet deployment, whose validator set is expected to grow and shrink over time). The Hub only calls out non-`prod` entries in its UI. |
| `seed_nodes` | Base URLs of this network's always-on anchor node(s) — the default bootstrap peer list a node configured for this `network_id` announces to (`POST /nodes/announce`) when it has no `AVALON_BOOTSTRAP_PEERS` of its own set. Reuses this file rather than a second committed list — an anchor node is exactly the "always-on node(s) each real deployment already plans to run" this file's entries already describe. Empty for a network with no anchor yet, or for the anchor's own entry (nothing to seed from). Also consumed by every official SDK's zero-URL `connect()`, which tries these (after the entry's own `server_url`) as its candidate list. |

Being a committed file in this repo *is* the integrity story: changing a
trusted entry goes through the same PR review and git history as any other
change here, not a quiet edit behind an API nobody watches. There is
deliberately no runtime "publish a new trust anchor" endpoint.

The [README](../../../../README.md#trusted-networks) renders the same file as a
table, not a second hand-maintained copy — `scripts/check-trust-anchors.mjs`
(run by `make check`) fails if the README table drifts from the JSON.

Clients do not bundle the list. Every official SDK fetches it at runtime from
`TRUST_ANCHORS_URL`
(`https://raw.githubusercontent.com/avalon-initiative/avalon-protocol/main/docs/trusted-networks.json`),
so a client always sees the current published file. When the list cannot be
fetched, the SDKs report the network as unreachable rather than falling back
to a stale copy.

## What this repo actually has today

There is no publicly deployed Avalon network yet — see the root
`.env.example`'s `AVALON_NETWORK_ID=avalon-dev-local` default.
`trusted-networks.json` accordingly ships exactly one entry
(`avalon-dev-local`), marked `"environment": "local-dev"` with an explicit
`notes` field explaining that its `verify_key` is a freely-generated key with
no real server behind it — checked in so the mechanism (file → README → Hub
verification) is exercised for real, not left as an unfilled stub. Adding a
second, real network later is the same PR-reviewed edit: append an entry with
the real `network_id`, that deployment's actual `AVALON_SETTLEMENT_VERIFY_KEY`
hex, and `environment` set to `dev`, `int`, or `prod` as appropriate.

Three real deployment tiers are supported, one `network_id` prefix each:
`avalon-dev-<name>` (`dev`, single-node), `avalon-int-<name>` (`int`, a 1-5
node interconnected test bed), and `avalon-mainnet-N` (`prod`). None of the
three has a real deployment behind it yet — `avalon-dev-local` above remains
the only entry in this file until one does.

`avalon-mainnet-N` is not a namespace open to multiple concurrent networks —
there is exactly one canonical mainnet at a time. `N` only increments for a
deliberate genesis reset of that one canonical chain, decided and merged by
the maintainers; it is never a way to stand up a second, competing mainnet.
Nothing stops a third party from running a server that claims
`AVALON_NETWORK_ID=avalon-mainnet-2` on its own — `network_id` has zero
authority by itself (see above) — but unless that exact `network_id` and its
real `verify_key` are merged into this file by the maintainers, a pinned
client shows it as an unknown/unverified network, not mainnet.

## Hub enforcement

`avalon-hub/apps/hub` does not implement any of the verification itself — it only
talks to the protocol layer through `@avalon-initiative/protocol-sdk`, whose
`AvalonClient.verifyNetwork()` fetches `GET /ledger/sth/latest`, matches the
STH's `network_id` against the trust-anchor list the SDK fetches at runtime, and
independently re-verifies the STH's Ed25519 signature against that entry's
`verify_key` (the same `(tree_size, root_hash, network_id, timestamp)` message
`crates/protocol/src/sth.rs::signing_message` defines).

- `src/composables/useNetworkTrust.ts` + `src/components/NetworkStatus.vue` —
  surfaced in the Hub shell sidebar (`src/views/HubShell.vue`), always visible, never
  buried in settings: which network the session is connected to, and one of
  four states —
  - **Verified** — `network_id` is pinned and the STH signature checks out.
  - **Key mismatch** — `network_id` is pinned but the signature does *not*
    verify against the pinned key. This is the impostor case: flagged
    clearly, never silently trusted.
  - **Unknown network** — the server's claimed `network_id` isn't in the
    pinned list at all. Labeled as unverified/custom, never treated as
    trusted by default.
  - **Unreachable** — the STH request itself failed, or the trust-anchor list could not be fetched.

  The same component lists every network in the fetched trust-anchor list, so which pinned entry the active connection corresponds to
  is explicit — not just a URL nobody can cross-check.
- **Switching is explicit, never silent.** `src/api/serverUrl.ts`'s
  `getServerUrl()`/`setServerUrl()` are the only reader/writer of which
  server the Hub talks to at runtime (persisted in `localStorage`, falling
  back to the build-time `VITE_AVALON_SERVER_URL` default). `NetworkStatus.vue`
  lists every trust-anchor entry with a `server_url` and a visible
  "Switch" action, plus an explicitly-labeled "custom network" option for a
  URL outside the pinned list — picking either persists the choice and
  reloads (an existing session's bearer token has no meaning against a
  different server, so a clean reload is the honest behavior rather than
  trying to carry state across the switch). A custom/unpinned network is
  never silently treated as verified: the same `useNetworkTrust` check runs
  against it and correctly reports **unknown network** unless its STH
  happens to verify against an already-pinned entry's key.

## Witness cosigning alongside the pinned key

The pinned `verify_key` above is still the log's own signer, and a client that checks
only that signature behaves exactly as before. Witness cosigning is additive: independent
nodes cosign heads they have checked for append-only growth, and a verifier that wants
more than one key's word accepts a head once a majority of its own known witnesses has
cosigned it (`avalon_protocol::cosigned_sth::verify_cosigned_tree_head`). With zero or one
known witness the rule reduces to the plain author-signature check, so no network is
special-cased. Full design and parameters: [`witness-cosigning.md`](./witness-cosigning.md).

What is and is not in place today:

- Nodes cosign, store, serve (`?witnesses=1`), gossip head summaries and verify mirrored
  and cross-shard heads by majority. The trust-anchor entry format is unchanged and
  carries no witness list: each verifier builds its own known list from discovery, with
  the bundled seed nodes as anchors whose witness keys come from their own announces.
- The SDKs and the Hub do not verify cosignatures yet; they check the author signature
  against the pinned key as described above. Until they do, a pinned client gets the
  same assurance it always had. The default `GET /ledger/sth/latest` body is unchanged,
  which is what keeps them working; `scripts/witness-drill.sh rollout` asserts that.
- A network can move onto witness cosigning without a reset and back off again:
  [`witness-policy-rollout.md`](../for-maintainers/witness-policy-rollout.md).

## What this does NOT solve

Client-side pinning stops a *server* from impersonating a network it doesn't
hold the key for. It does not stop a *maintainer* with legitimate commit
access to this repo from merging a bad `verify_key` in the first place, or a
compromised signing key from being republished here as if it were still
good. That is a governance/process problem — who can approve a change to
`trusted-networks.json`, and how a key compromise gets detected and rotated
— not something client-side code can fix by construction. This document
does not claim otherwise; treat a PR touching `trusted-networks.json` with
the scrutiny that claim deserves.

This is also covered by the Rust SDK: `AvalonClient::verify_network` fetches
`GET /ledger/sth/latest` and verifies it against the same
`docs/trusted-networks.json` list, fetched at runtime from `TRUST_ANCHORS_URL`.
The Rust SDK lives in the separate `avalon-sdks` repository — see
[`docs/projects/sdks/rust/README.md`](../../sdks/rust/README.md). Node
discovery (*finding* a node in the first place) is a separate, still-open
concern from trust anchors (*trusting* one once found).

Nor does this solve, or attempt to solve, the reverse direction: whether a
given *issuer's key* should be allowed to write on a given network. This
document is entirely about a client verifying which network a server
actually belongs to; the per-network issuer registration gate (see
[`achievements-and-attestations.md`](./achievements-and-attestations.md))
is the server-side admission gate for the opposite question. The two are
related only in that both exist because `network_id` identity has to be
established and enforced somewhere once `dev`/`int`/`mainnet` are real,
separate deployments; neither implements the other.

## Per-shard trust anchors

Sharded settlement authority (see [`settlement.md`](./settlement.md)) reopens
this document's own core claim — a bare identifier proves nothing — one level
down: a `shard_id` alone is exactly as untrustworthy as a bare `network_id`
was before this document's mechanism existed. This section covers how a
client (or a witness computing the cross-shard root) verifies a shard's STH
is genuinely signed by its claimed integrator, without inventing a second
trust-anchor list.

**Reuse issuer-key registration rather than duplicating it.** "This key
legitimately speaks for Integrator X" is already exactly what issuer-key
registration establishes — today, scoped to attestation issuance. A shard's
settlement-signing key is authorized the identical way: the integrator's root
key authors an `issuer.key_added` event with a `purpose: "shard_settlement"`
field (alongside today's implicit `"attestation"` purpose), through the same
root-authorizes-operational-key flow [`./issuers.md`](./issuers.md)
documents. Rotation and compromise reuse `issuer.key_revoked` unchanged — no
new revocation mechanism, no new key-domain lifecycle. This key is still a
distinct domain from an issuer's attestation-signing key (see `issuers.md`'s
"Three key domains, kept apart" table, extended below) — they're authorized
through the same event shape, never treated as interchangeable.

**No second static trust-anchor file.** `trusted-networks.json` stays exactly
what it is — one PR-reviewed entry per `network_id`, pinning the one key that
matters at that granularity: the **core shard's** verify key (the reserved
shard for identity/social/guild history). A per-integrator shard's trust
anchor is never a second committed file entry (shard creation happens at
integrator-registration velocity, not PR-review velocity) — it's the
`issuer.key_added(purpose: shard_settlement)` event itself, which is already
durable, signed, append-only history. Trusting a shard's key reduces to:
fetch an inclusion proof for that event against the **core shard's** STH (the
one thing actually pinned by `trusted-networks.json`), verify it the same way
any inclusion proof is verified today. One pinned key at the root; every
shard's authorization is transitively provable from it — not N
independently-pinned keys to maintain trust in.

**Composes with the cross-shard root.** A witness verifying the cross-shard
root already fetches every contributing shard's STH; this adds one more check
per shard: resolve that shard's currently-authorized `shard_settlement`
key(s) via the core-shard inclusion proof above, and confirm the shard's STH
signature verifies against one of them — catching an
internally-consistent-but-unauthorized shard (correct Merkle math, wrong or
revoked signer), not just aggregation math.

**Extends the key-domain table:**

| Key | Belongs to | Governs |
|---|---|---|
| identity key | the identity | mutations to the identity's own data |
| issuer key (`attestation`) | the integrator | attestations the integrator issues |
| issuer key (`shard_settlement`) | the integrator, as a shard operator | signing that integrator's own shard's log entries / tree heads |
| log operator key (core shard) | the core shard's operator | signing the core shard's log entries / tree heads |

**Implemented, one honest simplification from the original design.**
`avalon_protocol::integrators::KeyPurpose` (`Attestation`/`ShardSettlement`)
is a real field on `IssuerKey`/`issuer_keys`, authorized through the exact
same `POST /integrations/{slug}/keys` root-key-authorizes-operational-key flow
attestation keys already use — no second registry, purpose is just another
field on the same request/event/row. `IssuerKey::may_sign_attestations`/
`may_sign_shard_settlement` keep the two domains mechanically
non-interchangeable: a `shard_settlement` key can authenticate ordinary
challenge-response calls (purpose never gates authentication) but can never be
resolved as an attestation-signing key, and vice versa.
`avalon_server::cross_shard::resolve_shard_verify_keys_from_db` is the
witness-side composition: given a `shard_id` (`"{namespace}:{owner}"`), it
queries this node's own `issuer_keys` table directly for that integrator's
currently-unrevoked `shard_settlement` keys and verifies the shard's fetched
STH against them — falling back to the interim `AVALON_SHARD_VERIFY_KEYS`
static config only when nothing resolves from the database.

**Two sources, unioned.** `resolve_shard_verify_keys_from_db` returns the
union (without duplicates) of two derivations:

- The node's local `issuer_keys` table, written in the same transaction as the
  `issuer.key_added` event. This is the fast path on the registrar, the node
  where the integrator registered.
- Keys derived from the node's **mirrored core ledger**
  (`avalon_server::mirrored_shard_keys`). The mirror-watcher stores a core
  entry only after verifying its inclusion proof against a signature-checked
  STH from the pinned network key, so keys derived this way are rooted at the
  pinned core key rather than at a local table. The derivation reads the
  integrator's `game.registered` event (which fixes its id and category) and its
  `issuer.key_added` / `issuer.key_revoked` events in `seq` order. A key is
  valid when it was added with purpose `shard_settlement` and no revocation
  names its key id. `valid_until` is not applied, matching the local
  resolver, and a key of any other purpose is never returned.

Because of the mirrored derivation, a node that is not the registrar, and a
node whose core authority is unreachable, still verifies sibling shards as long
as it holds the relevant core history.

Limits: the node must mirror the core shard (`AVALON_MIRROR_PEERS` pointing at
the core authority); entries whose payload has been pruned cannot contribute,
and a pruned `game.registered` yields no keys because the integrator's
category and id are unknown. Cross-node login's "verified requester" check
uses the same two sources.

Live-verified: a `shard_settlement` key registers and reads back with its
purpose over the real API, purpose-gating is unit-tested directly
(`crates/protocol/src/integrators.rs`, `crates/server/tests/shard_trust_anchors.rs`),
and a live test resolves a key from seeded mirrored entries with no local row
and stops resolving it after a mirrored revocation.

## Self-certifying shard ids

Everything above resolves a shard's key by tracing it back, through the core
ledger, to a registered integrator — a real mechanism, but one that only
works once that integrator has registered at all, and only while the core
authority (or a mirror of it) is reachable. A second, permanent form exists
alongside it for exactly the case where neither is true: `node:<key-hash>`,
where `<key-hash>` is the lowercase hex SHA-256 digest of the shard's own
tree-head Ed25519 public key
(`avalon_protocol::shard_identity::derive_self_certifying_id`). A verifier
handed this id, a candidate key, and a signed tree head checks all three
purely locally
(`avalon_protocol::shard_identity::verify_self_certifying_tree_head`):
re-derive the id from the candidate key and compare, then verify the head's
signature against that same key. No core-ledger inclusion proof, no
`issuer.key_added` event, no database — the id is exactly as much trust
anchor as the key itself needs, and nothing more is ever consulted.

**Named and self-certifying ids are both first-class, permanent forms, told
apart by parsing alone.** `avalon_protocol::shard::parse_shard_id` returns a
distinct `ParsedShardId::SelfCertifying` variant for `node:<key-hash>`,
alongside the existing `Core`/`Owned` variants; `shard_authority` (the
`(namespace, owner)` extraction the registry-based resolvers above key off
of) returns `None` for it, the same as it already does for `core`. Neither
form is deprecated in favor of the other, and a network runs shards of both
kinds side by side.

**A name is a claim on top, never a gate underneath.** A self-certifying
shard has no human-readable name unless its own key signs one:
`avalon_protocol::shard_identity::NameBindingClaim` binds a name to a
self-certifying id, carrying the raw public key alongside the id so the
claim verifies in total isolation — no lookup of who "owns" the name, no
confirmation the id is even real beyond what the claim's own signature
proves. `sign_name_binding_claim`/`verify_name_binding_claim` are the
claim's sign/verify pair; resolving conflicting claims, proving a claim
against a domain, and any registry that indexes claims by name are the
naming layer's job, not this claim's. A self-certifying shard with no name
at all authors and is verified exactly the same as one with a claimed name.

## Domain-proven names

The naming layer `NameBindingClaim` deferred to above: proving a claim
against a domain, and resolving two conflicting claims for the same name,
both with no registry and no Avalon authority needing to be online —
`avalon_protocol::domain_proof`.

**Proof format.** The published proof value is the claim's own signature,
prefixed: `avalon-name-proof-v1:<claim.signature>`
(`domain_proof::expected_domain_proof`). Not a fresh hash of the key — the
signature already commits to `(self_certifying_id, public_key, name,
created_at)` as one unforgeable unit, so publishing it as the proof value
ties the domain directly to that exact claim. A different key, a different
name, or the same key claiming the same name at a different `created_at`
produces a different signature and therefore a different required proof
value, so there is nothing to replay a proof for. The domain publishes this
value at either:

- a well-known file, `https://<name>/.well-known/avalon-name-proof`
  (`domain_proof::WELL_KNOWN_PATH`), or
- a DNS TXT record at `_avalon-challenge.<name>`
  (`domain_proof::dns_txt_record_name`).

Either is sufficient. `domain_proof::verify_domain_proof(claim,
fetched_proof)` is the pure check: `claim` must verify on its own
(`verify_name_binding_claim`) and `fetched_proof` must equal
`expected_domain_proof(claim)`. Fetching the value is I/O and lives in
`crates/server` (`crate::name_claims`, well-known-file fetch only today —
the DNS TXT form is defined but not yet wired to a real resolver); the
verification logic itself takes no dependency on how the value was
obtained, matching how `crates/chain` (I/O) and `crates/protocol`
(verification) are split elsewhere in this codebase.

**Contested names.** Two different keys can each present a validly
domain-proven claim for the same name (a genuinely contested domain).
`domain_proof::contested_name_winner` resolves this deterministically:
earliest `created_at` wins, ties (down to the second) broken by the
lexicographically smaller `public_key`. Earliest-`created_at` rather than
"whichever proof was fetched most recently" on purpose — a domain's live
DNS/HTTP state can flap or be cached differently per verifier, so "most
recent fetch wins" would let two honest verifiers reach different answers
for the same pair of claims depending on network timing alone.
`created_at` is a fixed, signed field inside the claim itself, so every
verifier holding both claims computes the same answer regardless of when or
how either proof was fetched.

**Server wiring** (`crate::name_claims`): `POST
/shards/{self_certifying_id}/name-claims` accepts a signed claim, verifies
it, fetches and checks its domain proof, and — resolving any contest
against whatever is already stored for that name — records the result in
`name_claims` (migration `0074_name_claims`, one row per name, not
foreign-keyed to anything: a row is a cache of an already-proven fact, not
the source of trust for it). `GET /shards/name/{name}` and `GET
/shards/{self_certifying_id}/name-claims` are the read paths a client or
SDK resolves either direction through. Rate-limited per source
(`AVALON_NAME_CLAIM_RATE_LIMIT_PER_MINUTE`, default 5/minute, needing no
hoster configuration) on top of the blanket per-IP request ceiling, the
same `crate::topology_limits::EndpointLimits` pattern `/nodes/probe` and
`/nodes/trace` already use. The existing registry-based `game:<slug>`
integrator/issuer-key write paths (`POST /integrations`, `POST
/integrations/{slug}/keys`) gained the same per-source default (a separate,
more generous 60/minute default sized to survive a full local dev/test
run sharing one loopback source address, via
`AVALON_INTEGRATOR_REGISTRATION_RATE_LIMIT_PER_MINUTE`) without any change
to their resolution logic — this is a parallel, additive naming layer, and
never touches how `game:<slug>` names already resolve.

## Genesis reset / migration

A deliberate `avalon-mainnet-N` -> `avalon-mainnet-(N+1)` genesis reset —
always a rare, maintainer-decided event, never routine — does not start the
new network from nothing, and does not ask any issuer to re-sign anything.
Since attestation/event signatures are deliberately network-agnostic, a
signature that verified on `mainnet-N` verifies identically against
`mainnet-(N+1)` without modification; the only two things a migration
actually has to carry forward are the outgoing network's final ledger
checkpoint (for auditability — the new network's history is a documented
continuation, not an unexplained fresh start) and its issuer admission
registry, so no issuer — including one no longer reachable — has to take any
action to remain admitted on the new network.

`avalon_chain::migration::migrate_network` (`crates/chain/src/migration.rs`)
does this in one call, given a source pool (the outgoing network) and a
target pool (a fresh database with migrations applied but no genesis of
its own yet):

1. Reads the source's `network_id` and latest Signed Tree Head (or a
   zero-`tree_size` checkpoint if the source has a genesis but has never
   actually committed anything).
2. Establishes the target's own genesis (`PostgresSettlementProvider::connect`),
   failing fast if the target database already belongs to some other
   network — the same guarantee every other boot path against
   `chain_genesis` gets.
3. Records that checkpoint on the target, in
   `network_migration_checkpoints` — an append-only audit trail of which
   source network(s) this one was migrated from, queryable after the fact.
4. Bulk-carries every row of the source's `issuer_network_registrations`
   onto the target (`ON CONFLICT DO NOTHING`, so a retried run after a
   partial failure is always safe to just re-run).

`avalon migrate-network --target-database-url <url> --target-network-id <id>`
(`crates/cli/src/main.rs`) is the operator-facing entry point, run against
the outgoing network's own `DATABASE_URL` as the source. It never touches
the source beyond reading it — a migration can be attempted, inspected,
and re-run without any risk to the network being migrated from.

## Current implementation

- `docs/trusted-networks.json` — the canonical list (one `local-dev`
  `avalon-dev-local` entry, see above).
- The TypeScript SDK's `network/` module (`avalon-sdks` repository) — runtime
  trust-anchor fetching, STH message reconstruction, verification, and zero-URL discovery,
  unit-tested against real generated Ed25519 keypairs: a valid STH signature
  passes, a forged one or one signed by a different key is flagged, not
  silently accepted. The C# and Rust SDKs carry the same surface.
- `avalon-hub/apps/hub/src/composables/useNetworkTrust.ts`,
  `avalon-hub/apps/hub/src/components/NetworkStatus.vue` — the always-visible Hub-side
  UI, wired into `src/views/HubShell.vue`'s sidebar.
- README's ["Trusted networks"](../../../../README.md#trusted-networks) section.
- The Rust SDK's network module (`avalon-sdks` repository) — the SDK-side
  equivalent: `TrustAnchorEntry`, `fetch_trust_anchors()` (runtime fetch from
  `TRUST_ANCHORS_URL`, nothing bundled), `NetworkTrustStatus`'s four states, and
  `AvalonClient::verify_network`, unit-tested against a real generated
  Ed25519 keypair (valid, forged/wrong-key, and unpinned cases) plus a
  `wiremock`-backed end-to-end fetch test.
- `crates/protocol/src/network_trust.rs` — the server-side parsing
  (`TrustAnchorEntry`/`NetworkEnvironment`/`bundled_trust_anchors()`, no
  verification logic). Unlike the SDKs, the server compiles
  `docs/trusted-networks.json` in via `include_str!` for its own
  bootstrap-peer/anchor-node checks (`nodes.rs`, `cross_node_login.rs`,
  `mirror_watcher.rs`); the Rust SDK depends on nothing in this workspace and
  has its own fetching implementation.
- Not built: a "manually add a custom trust anchor" UI in the Hub (the Hub's
  own invariant is satisfied by clearly flagging an unpinned network as
  unverified rather than requiring a manual-add flow — see Invariants below).
- `crates/protocol/src/shard_identity.rs` — self-certifying shard ids
  (`derive_self_certifying_id`, `resolve_self_certifying_key`,
  `verify_self_certifying_tree_head`) and the `NameBindingClaim` sign/verify
  pair, both pure functions, unit-tested including forged id/key mismatches
  and a tampered claim. `avalon_server::core_author_guard::evaluate` is
  unchanged in behavior for a self-certifying `AVALON_OWN_SHARD_ID` (it
  already fell through to `NotApplicable`, the same as a named shard), now
  with a dedicated test and doc comment making that explicit rather than
  incidental.
- `crates/protocol/src/domain_proof.rs` — the naming layer this document's
  "Domain-proven names" section above describes: `expected_domain_proof`,
  `verify_domain_proof`, `contested_name_winner`, `WELL_KNOWN_PATH`,
  `dns_txt_record_name`, all pure and unit-tested (forged proofs, a proof
  for a different claim, and both tiebreak cases).
- `crates/server/src/name_claims.rs` — `POST
  /shards/{self_certifying_id}/name-claims`, `GET /shards/name/{name}`,
  `GET /shards/{self_certifying_id}/name-claims`, the well-known-file fetch
  (unit-tested against a `wiremock` server), and the per-source rate limit.
  Live-verified end to end (claim verification, domain-shape rejection,
  429s past the default rate limit, and a real Postgres round trip through
  the `name_claims` table). `crates/server/src/integrators.rs`'s
  `register_integrator`/`add_issuer_key` gained the matching per-source
  default rate limit; their own resolution/authorization logic is
  unchanged.
- Not yet built: wiring the DNS TXT proof form to a real resolver (only the
  well-known-file form is fetched today; the format is defined either way),
  and wiring a self-certifying shard's raw key into the cross-shard STH
  fetch path (today's `resolve_shard_verify_keys_from_db` only resolves
  named/registered shards) — both separate, later work.

## Invariants

- The trust-anchor list is the actual root of trust for network identity —
  `network_id` alone is never sufficient, anywhere in the Hub or SDK.
- A network not in the pinned list is never silently treated as trusted; the
  Hub labels it clearly as unverified rather than hiding the distinction.
- Changing what's published in `trusted-networks.json` goes through normal
  repo review — no path exists to alter it outside version control.
</content>
</invoke>
