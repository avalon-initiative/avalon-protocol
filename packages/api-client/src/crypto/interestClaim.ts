// Signed DHT interest claims (issue #610) — the browser-side minting half of
// `avalon_protocol::interest_claim` / `crates/server/src/interest.rs`. See
// that Rust module's own doc comment for the full design: a short assertion
// self-signed with the identity's own Ed25519 event-signing key (the same
// one `./continuation.ts` already uses), binding a guild channel or
// conversation id to the `base_url` of the node this connection is
// currently subscribing through — proving "the holder of this identity's
// signing key wants this channel/conversation's traffic delivered to this
// specific node" to any other node that later verifies it.
//
// `base_url` must be signed, not sent alongside the signature — see the
// Rust module doc comment on why an attacker could otherwise republish a
// legitimate member's own valid claim under a different `base_url` and
// redirect their real chat traffic. That's also why this only mints once a
// connection has actually heard the server's own `node_info` message
// (`ChatServerMessage::NodeInfo` in `crates/server/src/chat.rs`) — a
// browser has no other way to know which address its own node announces to
// peers (it may differ from whatever URL it just connected through, e.g.
// behind a load balancer).
import { signWithKey } from './signingKey'

/** Must match `avalon_protocol::interest_claim::DEFAULT_TTL_SECONDS`. */
const DEFAULT_TTL_SECONDS = 24 * 60 * 60

export type ClaimedScope =
  | { kind: 'channel'; channelId: string }
  | { kind: 'conversation'; conversationId: string }

function toHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('')
}

/**
 * The exact bytes an `InterestClaim`'s signature covers — must match
 * `avalon_protocol::interest_claim::signing_bytes` byte-for-byte. A mismatch
 * here means every claim this mints fails server-side verification (never a
 * security hole — the server independently recomputes and checks against
 * its own copy of this format).
 */
function signingBytes(
  identityId: string,
  signingKeyId: string,
  scope: ClaimedScope,
  baseUrl: string,
  nonce: string,
  issuedAt: Date,
  expiresAt: Date,
): Uint8Array {
  const scopeTag =
    scope.kind === 'channel' ? `channel:${scope.channelId}` : `conversation:${scope.conversationId}`
  const issuedAtSecs = Math.floor(issuedAt.getTime() / 1000)
  const expiresAtSecs = Math.floor(expiresAt.getTime() / 1000)
  return new TextEncoder().encode(
    `avalon:interest-claim:v1:${identityId}:${signingKeyId}:${scopeTag}:${baseUrl}:${nonce}:${issuedAtSecs}:${expiresAtSecs}`,
  )
}

/**
 * Mints a fresh, self-signed `InterestClaim`, wire-encoded as the plain
 * `serde_json` object `crates/server/src/chat.rs`'s `ChatClientMessage`
 * expects in its `claim` field — no `AVCT1.`-style prefix needed (a claim
 * only ever travels inside that JSON body, never a bearer-token slot).
 * Returns `null` when `secretKey` is absent — this connection simply has no
 * local signing key to sign with, a supported (if narrower) state; the
 * caller subscribes without a claim and gets local-only delivery.
 */
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
  const signature = signWithKey(secretKey, bytes)

  // Field names/shapes must match `avalon_protocol::interest_claim::InterestClaim`'s
  // `#[derive(Serialize)]` output exactly, including the internally-tagged
  // `scope` shape (`#[serde(tag = "kind", rename_all = "snake_case")]` on
  // `ClaimedScope`).
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
