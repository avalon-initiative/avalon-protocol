// The identity's Ed25519 event-signing key (issue #73) — distinct from the
// WebAuthn passkey, which lives entirely with the browser/platform and never
// passes through this module. See docs/architecture/identity.md.
//
// Storage: plain `localStorage`, unencrypted, keyed by identity id. This is a
// deliberate milestone-1 stopgap, not a default anyone should assume is
// final — see issue #122 for the actual planned design (a device-registration
// / linked-device grant model, #135, is the *primary* path off this browser;
// this module's mnemonic derivation, #134, is the disaster-recovery
// fallback underneath it — #122 decided to build both, not either/or).
//
// Key derivation (#134): the secret key is never random on its own anymore —
// it's deterministically derived from a BIP39 mnemonic phrase, the same
// wordlist/entropy-encoding standard every crypto wallet already uses. Any
// device that has the phrase can re-derive the exact same key entirely
// offline, with no server round-trip: the server only ever sees the
// resulting *public* key (identical to before this change), never the
// phrase or the private key.
import { ed25519 } from '@noble/curves/ed25519'
import { sha256 } from '@noble/hashes/sha256'
import { concatBytes } from '@noble/hashes/utils'
import { generateMnemonic, mnemonicToSeedSync, validateMnemonic } from '@scure/bip39'
import { wordlist } from '@scure/bip39/wordlists/english'

const STORAGE_PREFIX = 'avalon:signingKey:'

/// Domain-separation label for deriving a 32-byte Ed25519 seed out of
/// BIP39's 64-byte PBKDF2 seed. Versioned explicitly (`v1`) rather than
/// silently reusing raw BIP39 seed bytes: BIP39 seeds are normally fed into
/// BIP32 HD derivation, which this repo doesn't use (there's exactly one
/// signing key per identity, not a hierarchy) — hashing with a distinct
/// label documents that this is Avalon's own derivation, not a
/// BIP32-compatible one, and keeps the door open to a different scheme
/// later under a `v2` label without breaking `v1`-derived keys.
const DERIVATION_LABEL = new TextEncoder().encode('avalon:signing-key-seed:v1')

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

/** A newly generated keypair plus the mnemonic it was derived from, so the caller can show it once. */
export interface GeneratedSigningKey extends SigningKeyPair {
  mnemonic: string
}

/** Turns a BIP39 mnemonic into the same Ed25519 keypair every time — see `DERIVATION_LABEL` above. */
export function deriveSigningKeyFromMnemonic(mnemonic: string): SigningKeyPair {
  const bip39Seed = mnemonicToSeedSync(mnemonic)
  const secretKey = sha256(concatBytes(bip39Seed, DERIVATION_LABEL))
  return { secretKey, publicKey: ed25519.getPublicKey(secretKey) }
}

/**
 * Generates a fresh BIP39 mnemonic, derives its Ed25519 keypair, and
 * persists the secret key for `identityId` — same storage shape as before
 * #134, just no longer random on its own. The mnemonic itself is never
 * stored anywhere; it's returned once so the caller (`CreateIdentity.vue`)
 * can show it to the player exactly once.
 */
export function generateAndStoreSigningKey(identityId: string): GeneratedSigningKey {
  const mnemonic = generateMnemonic(wordlist)
  const { secretKey, publicKey } = deriveSigningKeyFromMnemonic(mnemonic)
  localStorage.setItem(storageKey(identityId), toBase64(secretKey))
  return { secretKey, publicKey, mnemonic }
}

/**
 * Recovers a signing key from a previously saved mnemonic and persists it
 * for `identityId` — the "I'm on a new device" / "I cleared my browser
 * storage" path. Throws if `mnemonic` isn't a valid BIP39 phrase, so the
 * caller can show a real error instead of silently deriving nonsense from a
 * typo.
 */
export function recoverAndStoreSigningKey(identityId: string, mnemonic: string): SigningKeyPair {
  if (!validateMnemonic(mnemonic, wordlist)) {
    throw new Error('That recovery phrase is not valid — check it for typos and try again.')
  }
  const keyPair = deriveSigningKeyFromMnemonic(mnemonic)
  localStorage.setItem(storageKey(identityId), toBase64(keyPair.secretKey))
  return keyPair
}

/** Reads back a previously stored secret key, if this browser has one for `identityId`. */
export function loadSigningKey(identityId: string): Uint8Array | null {
  const stored = localStorage.getItem(storageKey(identityId))
  return stored ? fromBase64(stored) : null
}

/**
 * Generates a fresh Ed25519 keypair for a device-grant request (#135) —
 * deliberately *not* persisted here. The design this implements keeps a
 * requesting device's secret key in memory only until its grant is
 * approved; persisting it before that would mean a rejected/abandoned
 * request left an orphaned, never-authorized key sitting in storage.
 * Nothing about this keypair is derived from a mnemonic — it's a genuinely
 * fresh, device-specific key, unlike #134's identity-wide recovery key.
 */
export function generateGrantRequestKeyPair(): SigningKeyPair {
  return ed25519.keygen()
}

/**
 * Persists a keypair produced by `generateGrantRequestKeyPair` for
 * `identityId`, once (and only once) its device grant has actually been
 * approved server-side — the moment this device becomes a real, usable
 * signing identity rather than a pending request.
 */
export function storeSigningKey(identityId: string, secretKey: Uint8Array): void {
  localStorage.setItem(storageKey(identityId), toBase64(secretKey))
}

/**
 * The exact bytes a device grant approval's Ed25519 signature covers —
 * must match `device_grant_approval_signing_bytes` in
 * crates/server/src/devices.rs byte-for-byte. Binds the grant id and the
 * requested public key so a signature can never be replayed against a
 * different grant or a different requested key.
 */
export function deviceGrantApprovalSigningBytes(
  grantId: string,
  identityId: string,
  requestedSigningPublicKey: Uint8Array,
): Uint8Array {
  return new TextEncoder().encode(
    `avalon:device_grant.approved:v1:${grantId}:${identityId}:${toBase64(requestedSigningPublicKey)}`,
  )
}

export function signWithKey(secretKey: Uint8Array, message: Uint8Array): Uint8Array {
  return ed25519.sign(message, secretKey)
}

/**
 * Derives the public key for an already-known secret key — used to find
 * which server-side `identity_signing_keys` row *is* this device (matching
 * on public key, the only thing the server ever learns about a device's
 * key) without this device having to remember its own server-assigned id
 * separately.
 */
export function publicKeyFromSecretKey(secretKey: Uint8Array): Uint8Array {
  return ed25519.getPublicKey(secretKey)
}

export function bytesToBase64(bytes: Uint8Array): string {
  return toBase64(bytes)
}

export function base64ToBytes(value: string): Uint8Array {
  return fromBase64(value)
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
