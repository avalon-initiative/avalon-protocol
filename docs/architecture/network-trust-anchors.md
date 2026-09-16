# Network Trust Anchors

**`network_id` alone is never sufficient to trust a server.** It is a plain
string with zero cryptographic authority — anyone can stand up their own
`avalon-server`, set `AVALON_NETWORK_ID=avalon-mainnet-1` (the exact string a
real deployment uses), and serve fabricated history under that name. The only
thing that actually distinguishes the real network from an impostor is
whether its Signed Tree Heads verify against the *specific* Ed25519 public
key belonging to the real settlement operator
([#210](https://github.com/LunarVagabond/avalon-protocol/issues/210)/[#211](https://github.com/LunarVagabond/avalon-protocol/issues/211)).
This document covers where that key is published and how a client pins it —
the same role a browser's pinned CA root list or SSH's `known_hosts` plays.

## The trust-anchor list

[`../trusted-networks.json`](../trusted-networks.json) is the canonical,
versioned, publicly-published trust-anchor list. Each entry:

| Field | Meaning |
|---|---|
| `label` | Human-readable name for the network. |
| `network_id` | The exact string a server sets `AVALON_NETWORK_ID` to and bakes into its ledger's genesis ([#173](https://github.com/LunarVagabond/avalon-protocol/issues/173)). |
| `verify_key` | Hex-encoded Ed25519 public key — the public half of that network's settlement operator signing key (`AVALON_SETTLEMENT_VERIFY_KEY`, see `crates/chain/src/sth.rs` and `.env.example`). |
| `signing_key_id` | Which key generation this is, matching `SignedTreeHead.signing_key_id` — informational; a rotated key gets a new entry (or a documented rotation), not a silent overwrite of this one. |
| `environment` | Which tier this deployment is: `local-dev` (no real deployment — a freely-generated key checked in only to exercise the mechanism end to end), `dev` (a real, non-production, single-node deployment — infra on one machine), `int` (a real, non-production, 1-5 node interconnected test bed used to verify changes actually integrate across nodes before they reach mainnet), or `prod` (a real mainnet deployment, whose validator set is expected to grow and shrink over time — see [#40](https://github.com/LunarVagabond/avalon-protocol/issues/40)). The Hub only calls out non-`prod` entries in its UI. |

Being a committed file in this repo *is* the integrity story: changing a
trusted entry goes through the same PR review and git history as any other
change here, not a quiet edit behind an API nobody watches. There is
deliberately no runtime "publish a new trust anchor" endpoint.

The [README](../../README.md#trusted-networks) renders the same file as a
table, not a second hand-maintained copy — `apps/hub` (below) reads it
end to end at build time via `apps/hub/src/network/trustAnchors.ts`, and a
test (`apps/hub/src/network/trustAnchors.readme.test.ts`) fails if the README
table drifts from the JSON.

## What this repo actually has today

Milestone 1 has no publicly deployed Avalon network — see the root
`CLAUDE.md`/`.env.example`'s `AVALON_NETWORK_ID=avalon-dev-local` default.
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

`apps/hub`:

- `src/network/trustAnchors.ts` — the bundled trust-anchor list (generated
  at build/dev/test time from `docs/trusted-networks.json`, see that file's
  header comment in `vite.config.ts` — never hand-copied).
- `src/api/client.ts` — `getLatestSth()`, `GET /ledger/sth/latest`
  against whatever `VITE_AVALON_SERVER_URL` the Hub is built against
  (`src/api/client.ts`).
- `src/network/verifyNetwork.ts` — matches the fetched STH's `network_id`
  against the bundled list and, if found, independently re-verifies the
  STH's Ed25519 signature against that entry's `verify_key` using
  `@noble/curves/ed25519` (already a Hub dependency for the identity signing
  key, `src/crypto/signingKey.ts`) — the exact same
  `(tree_size, root_hash, network_id, timestamp)` message format
  `crates/chain/src/sth.rs::signing_message` defines, reproduced byte-for-byte
  in `src/network/sthMessage.ts`.
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

This is now also covered by the Rust SDK (`crates/sdk/src/network.rs`,
[#482](https://github.com/LunarVagabond/avalon-protocol/issues/482)):
`AvalonClient::verify_network` fetches `GET /ledger/sth/latest` and
verifies it against the same `docs/trusted-networks.json` list, embedded
directly into the crate rather than mirrored into a generated file the
way `apps/hub` needs to. [#91](https://github.com/LunarVagabond/avalon-protocol/issues/91)'s
node-discovery work is a separate, still-open concern: #91 is about
*finding* a node; this document (and #482's SDK-side check) is about
*trusting* one once found.

Nor does this solve, or attempt to solve, the reverse direction: whether a
given *issuer's key* should be allowed to write on a given network. This
document is entirely about a client verifying which network a server
actually belongs to; [#481](https://github.com/LunarVagabond/avalon-protocol/issues/481)
(implementing the ADR decided in [#479](https://github.com/LunarVagabond/avalon-protocol/issues/479))
is the server-side admission gate for the opposite question — see
`docs/architecture/achievements-and-attestations.md`'s "Today in the repo"
section. The two are related only in that both exist because `network_id`
identity has to be established and enforced somewhere once `dev`/`int`/
`mainnet` are real, separate deployments; neither implements the other.

## Today in the repo

- `docs/trusted-networks.json` — the canonical list (one `local-dev`
  `avalon-dev-local` entry, see above).
- `apps/hub/src/network/` — trust-anchor loading, STH message
  reconstruction, and verification (`trustAnchors.ts`, `sthMessage.ts`,
  `verifyNetwork.ts`, unit-tested in
  `apps/hub/src/network/verifyNetwork.test.ts` against a real generated
  Ed25519 keypair: a valid STH signature passes, a forged one or one signed
  by a different key is flagged, not silently accepted).
- `apps/hub/src/composables/useNetworkTrust.ts`,
  `apps/hub/src/components/NetworkStatus.vue` — the always-visible Hub-side
  UI, wired into `HubShell.vue`'s sidebar.
- `apps/hub/src/api/client.ts` — the Hub's first settlement/ledger API
  client (`GET /ledger/sth/latest`); none existed before this.
- README's ["Trusted networks"](../../README.md#trusted-networks) section.
- `crates/sdk/src/network.rs` — the SDK-side equivalent (#482):
  `TrustAnchorEntry`/`bundled_trust_anchors()` (embedded from
  `docs/trusted-networks.json` via `include_str!`, no generated mirror
  needed), `NetworkTrustStatus`'s four states, and
  `AvalonClient::verify_network`, unit-tested against a real generated
  Ed25519 keypair (valid, forged/wrong-key, and unpinned cases) plus a
  `wiremock`-backed end-to-end fetch test.
- Not built: a "manually add a custom trust anchor" UI in the Hub (the
  Hub's own invariant is satisfied by clearly flagging an unpinned network
  as unverified rather than requiring a manual-add flow — see Invariants
  below), and any server-side change — #210/#211 already built everything
  the server needed to expose.

## Invariants

- The trust-anchor list is the actual root of trust for network identity —
  `network_id` alone is never sufficient, anywhere in the Hub or SDK.
- A network not in the pinned list is never silently treated as trusted; the
  Hub labels it clearly as unverified rather than hiding the distinction.
- Changing what's published in `trusted-networks.json` goes through normal
  repo review — no path exists to alter it outside version control.

## Decisions and tickets

- [#232](https://github.com/LunarVagabond/avalon-protocol/issues/232) — this
  document's own ticket.
- [#210](https://github.com/LunarVagabond/avalon-protocol/issues/210)/[#211](https://github.com/LunarVagabond/avalon-protocol/issues/211) —
  the Signed Tree Heads and public read endpoints this pins.
- [#173](https://github.com/LunarVagabond/avalon-protocol/issues/173) —
  `network_id` genesis/hashing, the identity string this document adds real
  cryptographic weight to.
- [#39](https://github.com/LunarVagabond/avalon-protocol/issues/39)/[#40](https://github.com/LunarVagabond/avalon-protocol/issues/40) —
  the decisions that made STH signing real in the first place.
- [#91](https://github.com/LunarVagabond/avalon-protocol/issues/91) — SDK
  node discovery, the related-but-distinct "finding a node" problem.
- [#482](https://github.com/LunarVagabond/avalon-protocol/issues/482) —
  this document's SDK-side counterpart, `crates/sdk/src/network.rs`.
