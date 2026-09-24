// Per-identity local Ed25519 signing-key storage: plain localStorage,
// unencrypted, keyed by identity id. Key generation and mnemonic derivation live
// in `@avalon-initiative/protocol-sdk`; this file only stores, loads and clears
// the bytes.
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
