import { describe, expect, it } from 'vitest'
import { canonicalMessage, generateSigningKey, sign, verify } from './signing.js'

describe('canonicalMessage', () => {
  it('matches the server shape byte-for-byte', () => {
    const message = canonicalMessage('guild.transfer_ownership', ['g1', 'from1', 'to1'])
    expect(new TextDecoder().decode(message)).toBe('avalon:guild.transfer_ownership:v1:g1:from1:to1')
  })

  it('with no fields is just the tag', () => {
    const message = canonicalMessage('integration.connect', [])
    expect(new TextDecoder().decode(message)).toBe('avalon:integration.connect:v1')
  })
})

describe('sign/verify', () => {
  it('produces a signature that verifies against the same canonical message', () => {
    const { secretKey, publicKey } = generateSigningKey()
    const message = canonicalMessage('passkey.revoke_last', ['p1', 'i1'])
    const signature = sign(secretKey, message)
    expect(verify(publicKey, message, signature)).toBe(true)
  })

  it('does not verify against a different message', () => {
    const { secretKey, publicKey } = generateSigningKey()
    const signature = sign(secretKey, canonicalMessage('passkey.revoke_last', ['p1', 'i1']))
    expect(verify(publicKey, canonicalMessage('passkey.revoke_last', ['p2', 'i1']), signature)).toBe(false)
  })
})
