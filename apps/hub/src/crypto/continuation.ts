// Session-continuation tokens (issue #525, Part 2 of #521's decision) —
// the browser-side minting half of `avalon_protocol::continuation` /
// `crates/server/src/continuation.rs`. A short-lived, self-signed
// assertion that lets an already-logged-in identity keep working against a
// *different* node than the one that issued its opaque session token —
// see this module's own exports' doc comments and
// docs/architecture/identity.md's "Session continuation across nodes"
// section for the full design.
//
// Never a login credential by itself (#122's decided separation,
// unchanged here): this only ever extends an already-established session.
// Nothing here decides *when* to mint one — see `api/client.ts::request`'s
// 401-retry, the actual "detect my node is unreachable, reconnect
// elsewhere" trigger.
import { signWithKey } from './signingKey'

/** Must match `avalon_protocol::continuation::WIRE_PREFIX` byte-for-byte. */
export const WIRE_PREFIX = 'AVCT1.'

/**
 * Must match `avalon_protocol::continuation::DEFAULT_TTL_SECONDS` — the
 * server independently re-checks `expires_at` itself (and rejects a
 * validity window longer than this), so a longer value here would just
 * mean every minted token gets rejected, not a longer-lived one.
 */
const DEFAULT_TTL_SECONDS = 60

function toHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('')
}

// Standard base64 (via btoa, same as signingKey.ts's toBase64) re-encoded
// as URL-safe, no padding — must match Rust's
// `base64::engine::general_purpose::URL_SAFE_NO_PAD`, distinct from the
// plain (padded) base64 this codebase otherwise uses for signature/key
// fields elsewhere (e.g. api/identity.ts's `event_signature`).
function toBase64UrlNoPad(bytes: Uint8Array): string {
  let binary = ''
  for (const byte of bytes) {
    binary += String.fromCharCode(byte)
  }
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
}

/**
 * The exact bytes a continuation token's signature covers — must match
 * `avalon_protocol::continuation::signing_bytes` byte-for-byte (a
 * mismatch here means every token this mints fails server-side
 * verification, not a security hole — the server independently recomputes
 * and checks against its own copy of this format).
 */
function signingBytes(
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

/**
 * Mints a fresh session-continuation token, signed locally with
 * `secretKey` — no network round trip, no server involvement in minting
 * (only in verifying, on whichever node it's later presented to). Each
 * call produces a genuinely new token (a fresh nonce and validity
 * window), since a token's nonce may only ever be accepted once — see
 * `crates/server/src/continuation.rs`'s anti-replay check. Wire-encodes
 * directly to the `AVCT1.<base64url>` string that goes straight into an
 * `Authorization: Bearer` header, exactly like an opaque session token.
 */
export function mintContinuationToken(
  identityId: string,
  signingKeyId: string,
  secretKey: Uint8Array,
): string {
  const nonce = crypto.randomUUID()
  const issuedAt = new Date()
  const expiresAt = new Date(issuedAt.getTime() + DEFAULT_TTL_SECONDS * 1000)
  const bytes = signingBytes(identityId, signingKeyId, nonce, issuedAt, expiresAt)
  const signature = signWithKey(secretKey, bytes)

  // Field names/shapes must match `avalon_protocol::continuation::ContinuationToken`'s
  // `#[derive(Serialize, Deserialize)]` output exactly — `serde_json`
  // matches object fields by name, not position, so declaration order
  // here doesn't matter, only the names and value shapes do.
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
