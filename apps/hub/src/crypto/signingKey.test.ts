import { describe, expect, it } from 'vitest'
import {
  base64ToBytes,
  bytesToBase64,
  deriveSigningKeyFromMnemonic,
  deviceGrantApprovalSigningBytes,
  generateAndStoreSigningKey,
  generateGrantRequestKeyPair,
  identityCreatedSigningBytes,
  loadSigningKey,
  publicKeyFromSecretKey,
  recoverAndStoreSigningKey,
  signWithKey,
  storeSigningKey,
} from './signingKey'
import { ed25519 } from '@noble/curves/ed25519'
import { generateMnemonic } from '@scure/bip39'
import { wordlist } from '@scure/bip39/wordlists/english'

describe('identityCreatedSigningBytes', () => {
  it('matches the exact byte format crates/server/src/handlers.rs verifies against', () => {
    const bytes = identityCreatedSigningBytes(
      '11111111-1111-1111-1111-111111111111',
      'Avalon Player',
    )
    const text = new TextDecoder().decode(bytes)
    expect(text).toBe(
      'avalon:identity.created:v1:11111111-1111-1111-1111-111111111111:Avalon Player',
    )
  })
})

describe('signing key storage', () => {
  it('round-trips through localStorage and produces a verifiable signature', () => {
    const identityId = crypto.randomUUID()
    const { publicKey, secretKey } = generateAndStoreSigningKey(identityId)

    const loaded = loadSigningKey(identityId)
    expect(loaded).not.toBeNull()
    expect(bytesToBase64(loaded!)).toBe(bytesToBase64(secretKey))

    const message = identityCreatedSigningBytes(identityId, 'Avalon Player')
    const signature = signWithKey(secretKey, message)
    expect(ed25519.verify(signature, message, publicKey)).toBe(true)
  })

  it('returns null for an identity with no stored key', () => {
    expect(loadSigningKey(crypto.randomUUID())).toBeNull()
  })
})

// #134
describe('mnemonic derivation and recovery', () => {
  it('deriving the same phrase twice yields the same keypair', () => {
    const mnemonic = generateMnemonic(wordlist)
    const first = deriveSigningKeyFromMnemonic(mnemonic)
    const second = deriveSigningKeyFromMnemonic(mnemonic)
    expect(bytesToBase64(first.secretKey)).toBe(bytesToBase64(second.secretKey))
    expect(bytesToBase64(first.publicKey)).toBe(bytesToBase64(second.publicKey))
  })

  it('different phrases derive different keypairs', () => {
    const a = deriveSigningKeyFromMnemonic(generateMnemonic(wordlist))
    const b = deriveSigningKeyFromMnemonic(generateMnemonic(wordlist))
    expect(bytesToBase64(a.secretKey)).not.toBe(bytesToBase64(b.secretKey))
  })

  it('the derived public key matches what register/finish already expects — a plain 32-byte Ed25519 verifying key', () => {
    const { publicKey, secretKey } = deriveSigningKeyFromMnemonic(generateMnemonic(wordlist))
    expect(publicKey).toHaveLength(32)
    const message = identityCreatedSigningBytes(crypto.randomUUID(), 'Avalon Player')
    const signature = signWithKey(secretKey, message)
    expect(ed25519.verify(signature, message, publicKey)).toBe(true)
  })

  it('generateAndStoreSigningKey returns a mnemonic that re-derives the same key it stored', () => {
    const identityId = crypto.randomUUID()
    const generated = generateAndStoreSigningKey(identityId)
    const rederived = deriveSigningKeyFromMnemonic(generated.mnemonic)
    expect(bytesToBase64(rederived.secretKey)).toBe(bytesToBase64(generated.secretKey))
  })

  it('recoverAndStoreSigningKey re-populates storage for a fresh identity id from a saved phrase', () => {
    const identityId = crypto.randomUUID()
    expect(loadSigningKey(identityId)).toBeNull()

    const mnemonic = generateMnemonic(wordlist)
    const recovered = recoverAndStoreSigningKey(identityId, mnemonic)

    const loaded = loadSigningKey(identityId)
    expect(loaded).not.toBeNull()
    expect(bytesToBase64(loaded!)).toBe(bytesToBase64(recovered.secretKey))
  })

  it('rejects an invalid recovery phrase rather than silently deriving nonsense', () => {
    expect(() =>
      recoverAndStoreSigningKey(crypto.randomUUID(), 'not a real bip39 phrase at all'),
    ).toThrow(/not valid/)
  })
})

// #135
describe('device grant helpers', () => {
  it('generateGrantRequestKeyPair produces a valid, usable Ed25519 keypair', () => {
    const { publicKey, secretKey } = generateGrantRequestKeyPair()
    const message = new TextEncoder().encode('a test message')
    const signature = signWithKey(secretKey, message)
    expect(ed25519.verify(signature, message, publicKey)).toBe(true)
  })

  it('generateGrantRequestKeyPair does not persist anything to localStorage', () => {
    const identityId = crypto.randomUUID()
    generateGrantRequestKeyPair()
    expect(loadSigningKey(identityId)).toBeNull()
  })

  it('storeSigningKey persists a keypair generated independently of it', () => {
    const identityId = crypto.randomUUID()
    const { secretKey } = generateGrantRequestKeyPair()
    storeSigningKey(identityId, secretKey)
    const loaded = loadSigningKey(identityId)
    expect(loaded).not.toBeNull()
    expect(bytesToBase64(loaded!)).toBe(bytesToBase64(secretKey))
  })

  it('publicKeyFromSecretKey matches the public key produced at generation time', () => {
    const { publicKey, secretKey } = generateGrantRequestKeyPair()
    expect(bytesToBase64(publicKeyFromSecretKey(secretKey))).toBe(bytesToBase64(publicKey))
  })

  it('base64ToBytes round-trips with bytesToBase64', () => {
    const { publicKey } = generateGrantRequestKeyPair()
    expect(bytesToBase64(base64ToBytes(bytesToBase64(publicKey)))).toBe(bytesToBase64(publicKey))
  })

  it('deviceGrantApprovalSigningBytes matches the exact byte format crates/server/src/devices.rs verifies against', () => {
    const grantId = '22222222-2222-2222-2222-222222222222'
    const identityId = '11111111-1111-1111-1111-111111111111'
    const key = new Uint8Array([1, 2, 3, 4])
    const bytes = deviceGrantApprovalSigningBytes(grantId, identityId, key)
    const text = new TextDecoder().decode(bytes)
    expect(text).toBe(
      `avalon:device_grant.approved:v1:${grantId}:${identityId}:${bytesToBase64(key)}`,
    )
  })

  it('deviceGrantApprovalSigningBytes differs for a different grant id', () => {
    const identityId = crypto.randomUUID()
    const key = new Uint8Array([1, 2, 3])
    const a = deviceGrantApprovalSigningBytes(crypto.randomUUID(), identityId, key)
    const b = deviceGrantApprovalSigningBytes(crypto.randomUUID(), identityId, key)
    expect(bytesToBase64(a)).not.toBe(bytesToBase64(b))
  })
})
