// The identity's Ed25519 event-signing key (issue #73) — distinct from the
// WebAuthn passkey, which lives entirely with the browser/platform and never
// passes through this module. See docs/architecture/identity.md.
//
// Storage: plain `localStorage`, unencrypted, keyed by identity id. This is a
// deliberate milestone-1 stopgap, not a default anyone should assume is
// final — see issue #122 for the actual planned design (a device-registration
// / linked-device grant model, not a hardware key). Losing this key today
// means the identity can never sign another event from this browser; nothing
// currently depends on that beyond the one-time identity.created signature,
// but that will stop being true as more event kinds require self-attribution.
import { ed25519 } from '@noble/curves/ed25519'

const STORAGE_PREFIX = 'avalon:signingKey:'

export interface SigningKeyPair {
  secretKey: Uint8Array
  publicKey: Uint8Array
}

function storageKey(identityId: string): string {
  return `${STORAGE_PREFIX}${identityId}`
}

function toBase64(bytes: Uint8Array): string {
  let binary = ''
  for (const byte of bytes) {
    binary += String.fromCharCode(byte)
  }
  return btoa(binary)
}

function fromBase64(value: string): Uint8Array {
  const binary = atob(value)
  const bytes = new Uint8Array(binary.length)
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i)
  }
  return bytes
}

/** Generates a fresh Ed25519 keypair and persists the secret key for `identityId`. */
export function generateAndStoreSigningKey(identityId: string): SigningKeyPair {
  const { secretKey, publicKey } = ed25519.keygen()
  localStorage.setItem(storageKey(identityId), toBase64(secretKey))
  return { secretKey, publicKey }
}

/** Reads back a previously stored secret key, if this browser has one for `identityId`. */
export function loadSigningKey(identityId: string): Uint8Array | null {
  const stored = localStorage.getItem(storageKey(identityId))
  return stored ? fromBase64(stored) : null
}

export function signWithKey(secretKey: Uint8Array, message: Uint8Array): Uint8Array {
  return ed25519.sign(message, secretKey)
}

export function bytesToBase64(bytes: Uint8Array): string {
  return toBase64(bytes)
}

/**
 * The exact bytes an `identity.created` claim's Ed25519 signature covers —
 * must match `identity_created_signing_bytes` in
 * crates/server/src/handlers.rs byte-for-byte. Deliberately a small,
 * explicit, versioned format rather than the full protocol event envelope.
 */
export function identityCreatedSigningBytes(identityId: string, displayName: string): Uint8Array {
  return new TextEncoder().encode(`avalon:identity.created:v1:${identityId}:${displayName}`)
}
