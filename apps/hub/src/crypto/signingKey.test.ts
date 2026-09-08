import { describe, expect, it } from 'vitest'
import {
  bytesToBase64,
  generateAndStoreSigningKey,
  identityCreatedSigningBytes,
  loadSigningKey,
  signWithKey,
} from './signingKey'
import { ed25519 } from '@noble/curves/ed25519'

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
