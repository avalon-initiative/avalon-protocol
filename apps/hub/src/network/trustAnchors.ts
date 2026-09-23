// Issue #232: the Hub's copy of Avalon's published trust-anchor list —
// `docs/trusted-networks.json` at the repo root is the one canonical file
// (integrity comes from it being a normal, PR-reviewed, git-tracked file);
// `vite.config.ts` mirrors it into `src/generated/` on every dev/build/test
// run so this module can import it as a plain, in-`src` JSON module without
// hand-copying it or reaching outside apps/hub's own TypeScript project.
// See docs/architecture/network-trust-anchors.md for the full model.
import trustedNetworks from '../generated/trusted-networks.json'

export interface TrustAnchorEntry {
  label: string
  network_id: string
  // Lowercase hex-encoded Ed25519 public key — the settlement operator's
  // STH verify key for this network (see crates/chain/src/sth.rs).
  verify_key: string
  signing_key_id: string
  // The server URL this network is reachable at — what the network
  // selector switches `apps/hub/src/api/client.ts`'s base URL to
  // when a viewer picks this entry. Optional: a trust-anchor entry's *key*
  // is what matters for verification, so an entry can be published to pin
  // a key ahead of its deployment having a known public URL yet.
  server_url?: string
  // Which tier this deployment is: 'local-dev' (no real deployment, a
  // freely-generated key checked in to exercise the mechanism end to end —
  // formerly the boolean `placeholder` flag), 'dev' (a real but non-production
  // single-node deployment), 'int' (a real, non-production multi-node
  // deployment used to test that changes actually integrate across nodes),
  // or 'prod' (a real mainnet deployment).
  environment: 'local-dev' | 'dev' | 'int' | 'prod'
  // Issue #362: base URLs of this network's always-on anchor node(s) — the
  // default bootstrap peers a node for this network_id announces to when
  // it has no bootstrap peers of its own configured. Not consumed by the
  // Hub itself (server-to-server discovery); mirrored here only so this
  // type stays field-for-field with the canonical JSON.
  seed_nodes?: string[]
  notes?: string
}

interface TrustedNetworksFile {
  version: number
  notes?: string
  networks: TrustAnchorEntry[]
}

const parsed = trustedNetworks as TrustedNetworksFile

/** Every network this Hub build was bundled with a pinned key for. */
export function getBundledTrustAnchors(): TrustAnchorEntry[] {
  return parsed.networks
}

/** The pinned entry for `networkId`, if this build knows about that network. */
export function findTrustAnchor(networkId: string): TrustAnchorEntry | undefined {
  return parsed.networks.find((entry) => entry.network_id === networkId)
}
