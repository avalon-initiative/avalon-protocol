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
table, not a second hand-maintained copy — every official SDK bundles the same
file (the TypeScript SDK, which `avalon-hub/apps/hub` consumes, mirrors it via its own
generate step), and a test (`avalon-hub/apps/hub/src/network/trustAnchors.readme.test.ts`)
fails if the README table drifts from the JSON.

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
STH's `network_id` against the SDK's bundled trust-anchor list, and
independently re-verifies the STH's Ed25519 signature against that entry's
`verify_key` (the same `(tree_size, root_hash, network_id, timestamp)` message
`crates/protocol/src/sth.rs::signing_message` defines).

- `src/composables/useNetworkTrust.ts` + `src/components/NetworkStatus.vue` —
  surfaced in the Hub shell sidebar (`HubShell.vue`), always visible, never
  buried in settings: which network the session is connected to, and one of
  four states —
  - **Verified** — `network_id` is pinned and the STH signature checks out.
  - **Key mismatch** — `network_id` is pinned but the signature does *not*
    verify against the pinned key. This is the impostor case: flagged
    clearly, never silently trusted.
  - **Unknown network** — the server's claimed `network_id` isn't in the
    pinned list at all. Labeled as unverified/custom, never treated as
    trusted by default.
  - **Unreachable** — the STH request itself failed.

  The same component lists every network the Hub build knows about (the
  bundled list), so which pinned entry the active connection corresponds to
  is explicit — not just a URL nobody can cross-check.
- **Switching is explicit, never silent.** `src/api/client.ts`'s
  `getServerUrl()`/`setServerUrl()` are the only reader/writer of which
  server the Hub talks to at runtime (persisted in `localStorage`, falling
  back to the build-time `VITE_AVALON_SERVER_URL` default). `NetworkStatus.vue`
  lists every bundled trust-anchor entry with a `server_url` and a visible
  "Switch" action, plus an explicitly-labeled "custom network" option for a
  URL outside the pinned list — picking either persists the choice and
  reloads (an existing session's bearer token has no meaning against a
  different server, so a clean reload is the honest behavior rather than
  trying to carry state across the switch). A custom/unpinned network is
  never silently treated as verified: the same `useNetworkTrust` check runs
  against it and correctly reports **unknown network** unless its STH
  happens to verify against an already-pinned entry's key.

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
`docs/trusted-networks.json` list, embedded directly into the crate rather
than mirrored into a generated file the way `avalon-hub/apps/hub` needs to. The Rust SDK
now lives in the separate `avalon-sdks` repository rather than this
workspace's own `crates/` — see
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
- The TypeScript SDK's `network/` module (`avalon-sdks` repository) — trust-anchor
  loading, STH message reconstruction, verification, and zero-URL discovery,
  unit-tested against real generated Ed25519 keypairs: a valid STH signature
  passes, a forged one or one signed by a different key is flagged, not
  silently accepted. The C# and Rust SDKs carry the same surface.
- `avalon-hub/apps/hub/src/composables/useNetworkTrust.ts`,
  `avalon-hub/apps/hub/src/components/NetworkStatus.vue` — the always-visible Hub-side
  UI, wired into `HubShell.vue`'s sidebar.
- README's ["Trusted networks"](../../../../README.md#trusted-networks) section.
- The Rust SDK's network module (`avalon-sdks` repository) — the SDK-side
  equivalent: `TrustAnchorEntry`/`bundled_trust_anchors()` (embedded from
  `docs/trusted-networks.json` via `include_str!`, no generated mirror
  needed), `NetworkTrustStatus`'s four states, and
  `AvalonClient::verify_network`, unit-tested against a real generated
  Ed25519 keypair (valid, forged/wrong-key, and unpinned cases) plus a
  `wiremock`-backed end-to-end fetch test.
- `crates/protocol/src/network_trust.rs` — a second, independent copy of just
  the parsing (`TrustAnchorEntry`/`NetworkEnvironment`/`bundled_trust_anchors()`,
  no verification logic), so `crates/server`'s own bootstrap-peer/anchor-node
  checks (`nodes.rs`, `cross_node_login.rs`, `mirror_watcher.rs`) don't need a
  dependency on the client SDK now that the SDK lives in a separate
  repository. Both copies parse the same `docs/trusted-networks.json`; kept
  as two hand-synced copies rather than shared code on purpose, matching the
  decision to make the Rust SDK depend on nothing else in this workspace.
- Not built: a "manually add a custom trust anchor" UI in the Hub (the Hub's
  own invariant is satisfied by clearly flagging an unpinned network as
  unverified rather than requiring a manual-add flow — see Invariants below).

## Invariants

- The trust-anchor list is the actual root of trust for network identity —
  `network_id` alone is never sufficient, anywhere in the Hub or SDK.
- A network not in the pinned list is never silently treated as trusted; the
  Hub labels it clearly as unverified rather than hiding the distinction.
- Changing what's published in `trusted-networks.json` goes through normal
  repo review — no path exists to alter it outside version control.
</content>
</invoke>
