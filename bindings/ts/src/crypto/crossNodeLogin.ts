// Cross-node login grants — mints
// `avalon_protocol::cross_node_login::CrossNodeLoginGrant` /
// `crates/server/src/cross_node_login.rs` locally. A free function rather
// than an AccountSession method: apps/hub's own call site
// (CrossNodeLogin.vue) mints straight from locally-held
// identityId/signingKeyId/secretKey material, not a full AccountSession
// against the destination node. See
// packages/api-client/src/crypto/crossNodeLogin.ts's own doc comment for
// the full design and why destinationBaseUrl/requestingContext are signed,
// not sent alongside the signature — a grant approved for one destination
// must never verify successfully against a different one.
import { sign } from './signing.js'

/** Must match `avalon_protocol::cross_node_login::DEFAULT_TTL_SECONDS`. */
export const DEFAULT_TTL_SECONDS = 60

function toHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('')
}

/** Wire-identical to
 * `avalon_protocol::cross_node_login::CrossNodeLoginGrant` — field names
 * and shapes must match exactly, since the server deserializes this
 * directly into that Rust type. */
export interface CrossNodeLoginGrant {
  identity_id: string
  signing_key_id: string
  destination_base_url: string
  requesting_context: string
  nonce: string
  issued_at: string
  expires_at: string
  signature: string
}

/** The exact bytes a grant's signature covers — must match
 * `avalon_protocol::cross_node_login::signing_bytes` byte-for-byte. */
export function signingBytes(
  identityId: string,
  signingKeyId: string,
  destinationBaseUrl: string,
  requestingContext: string,
  nonce: string,
  issuedAt: Date,
  expiresAt: Date,
): Uint8Array {
  const issuedAtSecs = Math.floor(issuedAt.getTime() / 1000)
  const expiresAtSecs = Math.floor(expiresAt.getTime() / 1000)
  return new TextEncoder().encode(
    `avalon:cross-node-login:v1:${identityId}:${signingKeyId}:${destinationBaseUrl}:` +
      `${requestingContext}:${nonce}:${issuedAtSecs}:${expiresAtSecs}`,
  )
}

/** Mints a fresh, self-signed `CrossNodeLoginGrant` — no network round
 * trip. A fresh nonce/validity window every call, since a nonce may only
 * ever be accepted once (anti-replay, server-side). */
export function mintCrossNodeLoginGrant(
  identityId: string,
  signingKeyId: string,
  secretKey: Uint8Array,
  destinationBaseUrl: string,
  requestingContext: string,
): CrossNodeLoginGrant {
  const nonce = crypto.randomUUID()
  const issuedAt = new Date()
  const expiresAt = new Date(issuedAt.getTime() + DEFAULT_TTL_SECONDS * 1000)
  const bytes = signingBytes(
    identityId,
    signingKeyId,
    destinationBaseUrl,
    requestingContext,
    nonce,
    issuedAt,
    expiresAt,
  )
  const signature = sign(secretKey, bytes)

  return {
    identity_id: identityId,
    signing_key_id: signingKeyId,
    destination_base_url: destinationBaseUrl,
    requesting_context: requestingContext,
    nonce,
    issued_at: issuedAt.toISOString(),
    expires_at: expiresAt.toISOString(),
    signature: toHex(signature),
  }
}
