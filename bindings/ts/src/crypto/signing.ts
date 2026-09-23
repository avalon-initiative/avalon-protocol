// Ed25519 event-signing primitives — reimplemented here rather
// than imported from packages/api-client/src/crypto/signingKey.ts, since
// this SDK's hard invariant is zero dependency on that package. Same library
// (@noble/curves), same wire shapes.
import { ed25519 } from '@noble/curves/ed25519'

export interface SigningKeyPair {
  secretKey: Uint8Array
  publicKey: Uint8Array
}

export function generateSigningKey(): SigningKeyPair {
  return ed25519.keygen()
}

export function publicKeyFromSecretKey(secretKey: Uint8Array): Uint8Array {
  return ed25519.getPublicKey(secretKey)
}

export function sign(secretKey: Uint8Array, message: Uint8Array): Uint8Array {
  return ed25519.sign(message, secretKey)
}

export function verify(publicKey: Uint8Array, message: Uint8Array, signature: Uint8Array): boolean {
  return ed25519.verify(signature, message, publicKey)
}

// btoa/atob (not Buffer, which browsers don't have) so this module works
// unmodified in both a browser and Node — both provide these globals.
export function bytesToBase64(bytes: Uint8Array): string {
  let binary = ''
  for (const byte of bytes) {
    binary += String.fromCharCode(byte)
  }
  return btoa(binary)
}

export function base64ToBytes(value: string): Uint8Array {
  const binary = atob(value)
  const bytes = new Uint8Array(binary.length)
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i)
  }
  return bytes
}

/**
 * Builds the canonical `avalon:<action_tag>:v1:<field1>:<field2>:...` byte
 * string a signature-required action signs — must match
 * `signature_gate::canonical_message` in `crates/server/src/signature_gate.rs`
 * byte-for-byte (see docs/architecture/identity.md's #697/#698 sections).
 */
export function canonicalMessage(actionTag: string, fields: string[]): Uint8Array {
  const parts = [`avalon:${actionTag}:v1`, ...fields]
  return new TextEncoder().encode(parts.join(':'))
}

/** The `signing_key_id`/`signature` pair every signature-required request
 * body carries — both `undefined` (serialized as JSON `null`) when the
 * session holds no local signing key. */
export interface SignatureFields {
  signing_key_id: string | null
  signature: string | null
}

/** Must produce exactly the bytes `avalon-server`'s
 * `handlers::identity_created_signing_bytes` reconstructs. */
export function identityCreatedSigningBytes(identityId: string, displayName: string): Uint8Array {
  return new TextEncoder().encode(`avalon:identity.created:v1:${identityId}:${displayName}`)
}

/** Must match `device_grant_approval_signing_bytes` in
 * `crates/server/src/devices.rs` byte-for-byte. */
export function deviceGrantApprovalSigningBytes(
  grantId: string,
  identityId: string,
  requestedSigningPublicKeyB64: string,
): Uint8Array {
  return new TextEncoder().encode(
    `avalon:device_grant.approved:v1:${grantId}:${identityId}:${requestedSigningPublicKeyB64}`,
  )
}
