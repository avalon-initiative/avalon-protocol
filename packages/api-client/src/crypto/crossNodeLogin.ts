// Cross-node login grants (epic #623, issue #639) — the browser-side
// minting half of `avalon_protocol::cross_node_login::CrossNodeLoginGrant` /
// `crates/server/src/cross_node_login.rs`. See that Rust module's own doc
// comment for the full design: a self-signed assertion, with the identity's
// own Ed25519 event-signing key (the same one `./continuation.ts` and
// `./interestClaim.ts` already use), that a human just approved logging
// this identity into `destinationBaseUrl` — a node this browser's own
// session may have nothing to do with.
//
// `destinationBaseUrl`/`requestingContext` are signed, not sent alongside
// the signature — same reasoning `./interestClaim.ts`'s own doc comment
// gives for `base_url`: a grant approved for one destination must never
// verify successfully against a different one.
import { signWithKey } from './signingKey'

/** Must match `avalon_protocol::cross_node_login::DEFAULT_TTL_SECONDS`. */
const DEFAULT_TTL_SECONDS = 60

function toHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('')
}

/**
 * The exact bytes a `CrossNodeLoginGrant`'s signature covers — must match
 * `avalon_protocol::cross_node_login::signing_bytes` byte-for-byte (a
 * mismatch here means every grant this mints fails server-side
 * verification, not a security hole — the server independently recomputes
 * and checks against its own copy of this format).
 */
function signingBytes(
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

/** Wire-identical to `avalon_protocol::cross_node_login::CrossNodeLoginGrant`
 * — field names and shapes must match exactly, since the server
 * deserializes this directly into that Rust type. */
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

/**
 * Mints a fresh, self-signed `CrossNodeLoginGrant` — no network round trip,
 * no server involvement in minting (only in verifying, on whichever node
 * this is submitted to). Each call produces a genuinely new grant (a fresh
 * nonce and validity window), since a grant's nonce may only ever be
 * accepted once — see `crates/server/src/cross_node_login.rs`'s anti-replay
 * check.
 */
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
  const signature = signWithKey(secretKey, bytes)

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
