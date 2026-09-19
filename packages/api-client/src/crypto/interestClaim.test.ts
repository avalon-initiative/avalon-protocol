import { describe, expect, it } from 'vitest'
import { ed25519 } from '@noble/curves/ed25519'
import { mintInterestClaim } from './interestClaim'

interface DecodedClaim {
  identity_id: string
  signing_key_id: string
  scope: { kind: 'channel'; channel_id: string } | { kind: 'conversation'; conversation_id: string }
  base_url: string
  nonce: string
  issued_at: string
  expires_at: string
  signature: string
}

function decode(wire: string): DecodedClaim {
  return JSON.parse(wire) as DecodedClaim
}

function hexToBytes(hex: string): Uint8Array {
  const bytes = new Uint8Array(hex.length / 2)
  for (let i = 0; i < bytes.length; i++) {
    bytes[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16)
  }
  return bytes
}

describe('mintInterestClaim', () => {
  it('decodes to the exact field shape avalon_protocol::interest_claim::InterestClaim expects', () => {
    const secretKey = ed25519.keygen().secretKey
    const wire = mintInterestClaim(
      '11111111-1111-1111-1111-111111111111',
      'key-42',
      secretKey,
      { kind: 'channel', channelId: '22222222-2222-2222-2222-222222222222' },
      'https://node-a.example',
    )
    const claim = decode(wire)

    expect(claim.identity_id).toBe('11111111-1111-1111-1111-111111111111')
    expect(claim.signing_key_id).toBe('key-42')
    expect(claim.scope).toEqual({
      kind: 'channel',
      channel_id: '22222222-2222-2222-2222-222222222222',
    })
    expect(claim.base_url).toBe('https://node-a.example')
    expect(typeof claim.nonce).toBe('string')
    expect(new Date(claim.issued_at).getTime()).not.toBeNaN()
    expect(new Date(claim.expires_at).getTime()).not.toBeNaN()
    expect(claim.signature).toMatch(/^[0-9a-f]+$/)
  })

  it('encodes a conversation scope with the matching internally-tagged shape', () => {
    const secretKey = ed25519.keygen().secretKey
    const wire = mintInterestClaim(
      'id',
      'key',
      secretKey,
      { kind: 'conversation', conversationId: 'convo-1' },
      'https://node-a.example',
    )
    const claim = decode(wire)
    expect(claim.scope).toEqual({ kind: 'conversation', conversation_id: 'convo-1' })
  })

  it('expires exactly 24 hours after issued_at, matching DEFAULT_TTL_SECONDS', () => {
    const secretKey = ed25519.keygen().secretKey
    const wire = mintInterestClaim(
      'id',
      'key',
      secretKey,
      { kind: 'channel', channelId: 'chan-1' },
      'https://node-a.example',
    )
    const claim = decode(wire)
    const issued = new Date(claim.issued_at).getTime()
    const expires = new Date(claim.expires_at).getTime()
    expect(expires - issued).toBe(24 * 60 * 60 * 1000)
  })

  it('signs bytes in the exact format avalon_protocol::interest_claim::signing_bytes recomputes', () => {
    const secretKey = ed25519.keygen().secretKey
    const publicKey = ed25519.getPublicKey(secretKey)
    const wire = mintInterestClaim(
      'id-a',
      'key-b',
      secretKey,
      { kind: 'channel', channelId: 'chan-9' },
      'https://node-a.example',
    )
    const claim = decode(wire)

    const issuedSecs = Math.floor(new Date(claim.issued_at).getTime() / 1000)
    const expiresSecs = Math.floor(new Date(claim.expires_at).getTime() / 1000)
    const expectedBytes = new TextEncoder().encode(
      `avalon:interest-claim:v1:id-a:key-b:channel:chan-9:https://node-a.example:${claim.nonce}:${issuedSecs}:${expiresSecs}`,
    )

    const signature = hexToBytes(claim.signature)
    expect(ed25519.verify(signature, expectedBytes, publicKey)).toBe(true)
  })

  it('a signature does not verify once base_url is swapped for a different one', () => {
    // The whole point of #610's fix: base_url is signed, so a claim
    // observed in the DHT can't be republished under a different
    // destination without breaking the signature.
    const secretKey = ed25519.keygen().secretKey
    const publicKey = ed25519.getPublicKey(secretKey)
    const wire = mintInterestClaim(
      'real-identity',
      'key',
      secretKey,
      { kind: 'channel', channelId: 'chan-1' },
      'https://legitimate-node.example',
    )
    const claim = decode(wire)

    const issuedSecs = Math.floor(new Date(claim.issued_at).getTime() / 1000)
    const expiresSecs = Math.floor(new Date(claim.expires_at).getTime() / 1000)
    const redirectedBytes = new TextEncoder().encode(
      `avalon:interest-claim:v1:real-identity:key:channel:chan-1:https://attacker.example:${claim.nonce}:${issuedSecs}:${expiresSecs}`,
    )

    const signature = hexToBytes(claim.signature)
    expect(ed25519.verify(signature, redirectedBytes, publicKey)).toBe(false)
  })

  it('mints a fresh, distinct nonce on every call', () => {
    const secretKey = ed25519.keygen().secretKey
    const scope = { kind: 'channel' as const, channelId: 'chan-1' }
    const first = decode(mintInterestClaim('id', 'key', secretKey, scope, 'https://node-a.example'))
    const second = decode(mintInterestClaim('id', 'key', secretKey, scope, 'https://node-a.example'))
    expect(first.nonce).not.toBe(second.nonce)
  })
})
