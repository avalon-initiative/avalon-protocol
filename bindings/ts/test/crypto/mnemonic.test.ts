import { describe, expect, it } from 'vitest'
import { generateMnemonic } from '@scure/bip39'
import { wordlist } from '@scure/bip39/wordlists/english'
import { deriveSigningKeyFromMnemonic, generateMnemonicSigningKey, isValidMnemonic } from '../../src/crypto/mnemonic.js'

describe('deriveSigningKeyFromMnemonic', () => {
  it('deriving the same phrase twice yields the same keypair', () => {
    const mnemonic = generateMnemonic(wordlist)
    const first = deriveSigningKeyFromMnemonic(mnemonic)
    const second = deriveSigningKeyFromMnemonic(mnemonic)
    expect(first.secretKey).toEqual(second.secretKey)
    expect(first.publicKey).toEqual(second.publicKey)
  })

  it('different phrases derive different keypairs', () => {
    const a = deriveSigningKeyFromMnemonic(generateMnemonic(wordlist))
    const b = deriveSigningKeyFromMnemonic(generateMnemonic(wordlist))
    expect(a.secretKey).not.toEqual(b.secretKey)
  })

  it('derives a plain 32-byte Ed25519 secret key', () => {
    const { secretKey, publicKey } = deriveSigningKeyFromMnemonic(generateMnemonic(wordlist))
    expect(secretKey).toHaveLength(32)
    expect(publicKey).toHaveLength(32)
  })
})

describe('generateMnemonicSigningKey', () => {
  it('returns a mnemonic that re-derives the exact same key it generated', () => {
    const generated = generateMnemonicSigningKey()
    const rederived = deriveSigningKeyFromMnemonic(generated.mnemonic)
    expect(rederived.secretKey).toEqual(generated.secretKey)
    expect(rederived.publicKey).toEqual(generated.publicKey)
  })

  it('generates a valid BIP39 phrase', () => {
    const { mnemonic } = generateMnemonicSigningKey()
    expect(isValidMnemonic(mnemonic)).toBe(true)
  })
})

describe('isValidMnemonic', () => {
  it('rejects a malformed phrase', () => {
    expect(isValidMnemonic('not a real recovery phrase at all')).toBe(false)
  })
})
