// Signed DHT interest claims — the AccountSession-side
// minting half of `avalon_protocol::interest_claim` /
// `crates/server/src/interest.rs`. A short assertion self-signed with the
// identity's own Ed25519 event-signing key, binding a guild channel or
// conversation id to the `base_url` of the node a chat websocket
// connection is currently subscribing through. See
// packages/api-client/src/crypto/interestClaim.ts's own doc comment for
// the full design and the replay-safety reasoning behind signing
// `base_url` rather than sending it alongside the signature; this mirrors
// it, using `AccountSession`'s own in-memory signing key rather than a
// storage-adapter lookup.
import { sign } from './signing.js'

/** Must match `avalon_protocol::interest_claim::DEFAULT_TTL_SECONDS`. */
const DEFAULT_TTL_SECONDS = 24 * 60 * 60

export type ClaimedScope = { kind: 'channel'; channelId: string } | { kind: 'conversation'; conversationId: string }

function toHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('')
}

/** Must match `avalon_protocol::interest_claim::signing_bytes` byte-for-byte.
 * Exported so the conformance suite can assert this format
 * directly against the shared vectors, same as `continuation.ts`'s and
 * `crossNodeLogin.ts`'s own exported `signingBytes`. */
export function signingBytes(
  identityId: string,
  signingKeyId: string,
  scope: ClaimedScope,
  baseUrl: string,
  nonce: string,
  issuedAt: Date,
  expiresAt: Date,
): Uint8Array {
  const scopeTag = scope.kind === 'channel' ? `channel:${scope.channelId}` : `conversation:${scope.conversationId}`
  const issuedAtSecs = Math.floor(issuedAt.getTime() / 1000)
  const expiresAtSecs = Math.floor(expiresAt.getTime() / 1000)
  return new TextEncoder().encode(
    `avalon:interest-claim:v1:${identityId}:${signingKeyId}:${scopeTag}:${baseUrl}:${nonce}:${issuedAtSecs}:${expiresAtSecs}`,
  )
}

/** Mints a fresh, self-signed `InterestClaim`, wire-encoded as the plain
 * JSON object `crates/server/src/chat.rs`'s `ChatClientMessage` expects in
 * its `claim` field — no `AVCT1.`-style prefix, a claim only ever travels
 * inside that JSON body, never a bearer-token slot. */
export function mintInterestClaim(
  identityId: string,
  signingKeyId: string,
  secretKey: Uint8Array,
  scope: ClaimedScope,
  baseUrl: string,
): string {
  const nonce = crypto.randomUUID()
  const issuedAt = new Date()
  const expiresAt = new Date(issuedAt.getTime() + DEFAULT_TTL_SECONDS * 1000)
  const bytes = signingBytes(identityId, signingKeyId, scope, baseUrl, nonce, issuedAt, expiresAt)
  const signature = sign(secretKey, bytes)

  // Field names/shapes must match `avalon_protocol::interest_claim::InterestClaim`'s
  // `#[derive(Serialize)]` output exactly, including the internally-tagged
  // `scope` shape.
  const claim = {
    identity_id: identityId,
    signing_key_id: signingKeyId,
    scope:
      scope.kind === 'channel'
        ? { kind: 'channel', channel_id: scope.channelId }
        : { kind: 'conversation', conversation_id: scope.conversationId },
    base_url: baseUrl,
    nonce,
    issued_at: issuedAt.toISOString(),
    expires_at: expiresAt.toISOString(),
    signature: toHex(signature),
  }
  return JSON.stringify(claim)
}
