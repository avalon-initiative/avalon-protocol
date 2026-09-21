// BIP39 mnemonic-derived signing keys (issue #134's recovery fallback) —
// reimplemented here from packages/api-client/src/crypto/signingKey.ts's
// `deriveSigningKeyFromMnemonic`/`generateAndStoreSigningKey`/
// `recoverAndStoreSigningKey`, since #701's own AccountSession registration
// (`AvalonClient.register`) generates a genuinely random key with no
// recovery phrase at all — a real gap relative to what Hub's own
// production `createIdentity` flow already does and needs to keep doing
// once it migrates onto this SDK (issue #712). Storage stays Hub's own
// concern (this module never touches localStorage) — only the pure
// mnemonic<->key derivation moves here.
import { ed25519 } from '@noble/curves/ed25519'
import { sha256 } from '@noble/hashes/sha256'
import { concatBytes } from '@noble/hashes/utils'
import { generateMnemonic, mnemonicToSeedSync, validateMnemonic } from '@scure/bip39'
import { wordlist } from '@scure/bip39/wordlists/english'
import type { SigningKeyPair } from './signing.js'

/** Domain-separation label for deriving a 32-byte Ed25519 seed out of
 * BIP39's 64-byte PBKDF2 seed — must match
 * packages/api-client/src/crypto/signingKey.ts's `DERIVATION_LABEL`
 * byte-for-byte, or a phrase recovered through this SDK would derive a
 * different key than the one Hub's existing users already have. */
const DERIVATION_LABEL = new TextEncoder().encode('avalon:signing-key-seed:v1')

/** Turns a BIP39 mnemonic into the same Ed25519 keypair every time. */
export function deriveSigningKeyFromMnemonic(mnemonic: string): SigningKeyPair {
  const bip39Seed = mnemonicToSeedSync(mnemonic)
  const secretKey = sha256(concatBytes(bip39Seed, DERIVATION_LABEL))
  return { secretKey, publicKey: ed25519.getPublicKey(secretKey) }
}

/** A newly generated keypair plus the mnemonic it was derived from, so the
 * caller can show it to the user exactly once — this module never
 * persists it anywhere itself. */
export interface GeneratedSigningKey extends SigningKeyPair {
  mnemonic: string
}

/** Generates a fresh BIP39 mnemonic and derives its Ed25519 keypair. */
export function generateMnemonicSigningKey(): GeneratedSigningKey {
  const mnemonic = generateMnemonic(wordlist)
  const { secretKey, publicKey } = deriveSigningKeyFromMnemonic(mnemonic)
  return { secretKey, publicKey, mnemonic }
}

/** `true` iff `mnemonic` is a well-formed BIP39 phrase — check before
 * calling `deriveSigningKeyFromMnemonic` on user input so a typo produces
 * a real error instead of silently deriving nonsense. */
export function isValidMnemonic(mnemonic: string): boolean {
  return validateMnemonic(mnemonic, wordlist)
}
