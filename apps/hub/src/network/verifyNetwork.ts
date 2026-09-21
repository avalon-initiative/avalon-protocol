// Issue #232's actual enforcement step: given a fetched Signed Tree Head
// and the bundled trust-anchor list, decide whether the server it came from
// is the real, pinned network it claims to be — never just displaying a
// `network_id` string and calling that trust. Mirrors
// `crates/chain/src/sth.rs::verify_tree_head`'s own contract: malformed hex
// or a wrong-length signature/key fails closed (`false`/a rejected status),
// never throws into the caller.
import { ed25519 } from '@noble/curves/ed25519'
import type { SignedTreeHeadResponse } from '@avalon/sdk'
import { signingMessage } from './sthMessage'
import type { TrustAnchorEntry } from './trustAnchors'

function hexToBytes(hex: string): Uint8Array | null {
  if (hex.length % 2 !== 0 || !/^[0-9a-fA-F]*$/.test(hex)) return null
  const bytes = new Uint8Array(hex.length / 2)
  for (let i = 0; i < bytes.length; i += 1) {
    bytes[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16)
  }
  return bytes
}

/**
 * Verifies `sth`'s Ed25519 signature against `verifyKeyHex` (lowercase hex,
 * 32 bytes) — `false` for any malformed input (bad hex, wrong-length
 * signature or key) as well as an outright-invalid signature, never throws.
 * The exact TypeScript-side counterpart to `verify_tree_head` in
 * `crates/chain/src/sth.rs`.
 */
export function verifyTreeHead(verifyKeyHex: string, sth: SignedTreeHeadResponse): boolean {
  const verifyKey = hexToBytes(verifyKeyHex)
  const signature = hexToBytes(sth.signature)
  if (!verifyKey || verifyKey.length !== 32) return false
  if (!signature || signature.length !== 64) return false
  try {
    const message = signingMessage(sth)
    return ed25519.verify(signature, message, verifyKey)
  } catch {
    // A malformed created_at, or anything else signingMessage/ed25519.verify
    // could throw on for attacker-influenced input — fail closed.
    return false
  }
}

export type NetworkTrustStatus =
  // The claimed network_id is pinned and the STH signature checks out —
  // this really is the network it says it is.
  | { kind: 'verified'; entry: TrustAnchorEntry }
  // The claimed network_id is pinned, but the signature does NOT verify
  // against the pinned key — the impostor case this ticket exists for.
  // Never silently downgraded to "unverified"; always its own status.
  | { kind: 'mismatch'; entry: TrustAnchorEntry; claimedNetworkId: string }
  // The claimed network_id isn't in the bundled trust-anchor list at all.
  | { kind: 'unknown-network'; claimedNetworkId: string }

/**
 * The invariant this ticket is actually for: `network_id` alone is never
 * sufficient. Given the STH a server actually returned and every network
 * this Hub build has a pinned key for, decides which of the three states
 * above applies — a caller must never treat anything but `'verified'` as
 * "connected to the real network."
 */
export function evaluateNetworkTrust(
  anchors: TrustAnchorEntry[],
  sth: SignedTreeHeadResponse,
): NetworkTrustStatus {
  const entry = anchors.find((candidate) => candidate.network_id === sth.network_id)
  if (!entry) {
    return { kind: 'unknown-network', claimedNetworkId: sth.network_id }
  }
  if (!verifyTreeHead(entry.verify_key, sth)) {
    return { kind: 'mismatch', entry, claimedNetworkId: sth.network_id }
  }
  return { kind: 'verified', entry }
}
