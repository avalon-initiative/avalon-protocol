// Per-identity local Ed25519 signing-key storage — plain localStorage,
// unencrypted, keyed by identity id. Ported from
// packages/api-client/src/crypto/signingKey.ts's own storage half as part
// of migrating off that package (issue #712); the actual key
// generation/mnemonic-derivation logic now lives in `@avalon/sdk`
// (crypto/mnemonic.ts) — this file only ever stores/loads/clears bytes,
// same storage key as before so an existing viewer's stored key keeps
// working unchanged across the migration.
const STORAGE_PREFIX = 'avalon:signingKey:'

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

/** Persists `secretKey` as `identityId`'s locally-held signing key. */
export function storeSigningKeySeed(identityId: string, secretKey: Uint8Array): void {
  localStorage.setItem(storageKey(identityId), toBase64(secretKey))
}

/** Reads back a previously stored secret key, if this browser has one for `identityId`. */
export function loadSigningKeySeed(identityId: string): Uint8Array | null {
  const stored = localStorage.getItem(storageKey(identityId))
  return stored ? fromBase64(stored) : null
}

/** Removes any locally-stored signing key for `identityId` — used on logout. */
export function clearSigningKeySeed(identityId: string): void {
  localStorage.removeItem(storageKey(identityId))
}
