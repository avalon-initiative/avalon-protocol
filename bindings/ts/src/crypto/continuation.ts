// Session-continuation tokens (issue #525) — the AccountSession-side
// minting half of `avalon_protocol::continuation` / `crates/server/src/
// continuation.rs`. A short-lived, self-signed assertion that lets an
// already-logged-in identity keep working against a *different* node than
// the one that issued its opaque session token. See
// docs/architecture/identity.md's "Session continuation across nodes"
// section, and packages/api-client/src/crypto/continuation.ts for the
// storage-adapter-based port this mirrors — simpler here since
// AccountSession already holds identityId/signingKeyId/secretKey in
// memory, with nothing to read out of a pluggable storage adapter.
import { sign } from './signing.js'

/** Must match `avalon_protocol::continuation::WIRE_PREFIX` byte-for-byte. */
export const WIRE_PREFIX = 'AVCT1.'

/** Must match `avalon_protocol::continuation::DEFAULT_TTL_SECONDS` — the
 * server independently re-checks `expires_at` itself. */
export const DEFAULT_TTL_SECONDS = 60

function toHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('')
}

// Standard base64 (via btoa) re-encoded as URL-safe, no padding — must
// match Rust's `base64::engine::general_purpose::URL_SAFE_NO_PAD`.
function toBase64UrlNoPad(bytes: Uint8Array): string {
  let binary = ''
  for (const byte of bytes) {
    binary += String.fromCharCode(byte)
  }
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
}

/** The exact bytes a continuation token's signature covers — must match
 * `avalon_protocol::continuation::signing_bytes` byte-for-byte. */
export function signingBytes(
  identityId: string,
  signingKeyId: string,
  nonce: string,
  issuedAt: Date,
  expiresAt: Date,
): Uint8Array {
  const issuedAtSecs = Math.floor(issuedAt.getTime() / 1000)
  const expiresAtSecs = Math.floor(expiresAt.getTime() / 1000)
  return new TextEncoder().encode(
    `avalon:continuation:v1:${identityId}:${signingKeyId}:${nonce}:${issuedAtSecs}:${expiresAtSecs}`,
  )
}

/** Mints a fresh session-continuation token, signed locally — no network
 * round trip. Wire-encodes directly to the `AVCT1.<base64url>` string that
 * goes straight into an `Authorization: Bearer` header, exactly like an
 * opaque session token. Never persisted back as this session's own
 * `_token`: single-use by design, since a token's nonce may only ever be
 * accepted once (`crates/server/src/continuation.rs`'s anti-replay check). */
export function mintContinuationToken(identityId: string, signingKeyId: string, secretKey: Uint8Array): string {
  const nonce = crypto.randomUUID()
  const issuedAt = new Date()
  const expiresAt = new Date(issuedAt.getTime() + DEFAULT_TTL_SECONDS * 1000)
  const bytes = signingBytes(identityId, signingKeyId, nonce, issuedAt, expiresAt)
  const signature = sign(secretKey, bytes)

  // Field names/shapes must match
  // `avalon_protocol::continuation::ContinuationToken`'s
  // `#[derive(Serialize, Deserialize)]` output exactly.
  const token = {
    identity_id: identityId,
    signing_key_id: signingKeyId,
    nonce,
    issued_at: issuedAt.toISOString(),
    expires_at: expiresAt.toISOString(),
    signature: toHex(signature),
  }
  const json = new TextEncoder().encode(JSON.stringify(token))
  return `${WIRE_PREFIX}${toBase64UrlNoPad(json)}`
}
