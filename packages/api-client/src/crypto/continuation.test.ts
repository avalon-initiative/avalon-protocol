import { describe, expect, it } from 'vitest'
import { ed25519 } from '@noble/curves/ed25519'
import { mintContinuationToken, WIRE_PREFIX } from './continuation'

interface DecodedToken {
  identity_id: string
  signing_key_id: string
  nonce: string
  issued_at: string
  expires_at: string
  signature: string
}

function decode(wire: string): DecodedToken {
  expect(wire.startsWith(WIRE_PREFIX)).toBe(true)
  const body = wire.slice(WIRE_PREFIX.length)
  const base64 = body.replace(/-/g, '+').replace(/_/g, '/')
  const padded = base64 + '='.repeat((4 - (base64.length % 4)) % 4)
  const json = atob(padded)
  return JSON.parse(json) as DecodedToken
}

function hexToBytes(hex: string): Uint8Array {
  const bytes = new Uint8Array(hex.length / 2)
  for (let i = 0; i < bytes.length; i++) {
    bytes[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16)
  }
  return bytes
}

describe('mintContinuationToken', () => {
  it('produces a wire string starting with the exact WIRE_PREFIX the server expects', () => {
    const secretKey = ed25519.keygen().secretKey
    const wire = mintContinuationToken('identity-1', 'key-1', secretKey)
    expect(wire.startsWith('AVCT1.')).toBe(true)
  })

  it('decodes to the exact field shape avalon_protocol::continuation::ContinuationToken expects', () => {
    const secretKey = ed25519.keygen().secretKey
    const wire = mintContinuationToken('11111111-1111-1111-1111-111111111111', 'key-42', secretKey)
    const token = decode(wire)

    expect(token.identity_id).toBe('11111111-1111-1111-1111-111111111111')
    expect(token.signing_key_id).toBe('key-42')
    expect(typeof token.nonce).toBe('string')
    expect(new Date(token.issued_at).getTime()).not.toBeNaN()
    expect(new Date(token.expires_at).getTime()).not.toBeNaN()
    expect(token.signature).toMatch(/^[0-9a-f]+$/)
  })

  it('expires exactly 60 seconds after issued_at, matching DEFAULT_TTL_SECONDS', () => {
    const secretKey = ed25519.keygen().secretKey
    const wire = mintContinuationToken('id', 'key', secretKey)
    const token = decode(wire)
    const issued = new Date(token.issued_at).getTime()
    const expires = new Date(token.expires_at).getTime()
    expect(expires - issued).toBe(60_000)
  })

  it('signs bytes in the exact format crates/server/src/continuation.rs::verify recomputes', () => {
    const secretKey = ed25519.keygen().secretKey
    const publicKey = ed25519.getPublicKey(secretKey)
    const wire = mintContinuationToken('id-a', 'key-b', secretKey)
    const token = decode(wire)

    const issuedSecs = Math.floor(new Date(token.issued_at).getTime() / 1000)
    const expiresSecs = Math.floor(new Date(token.expires_at).getTime() / 1000)
    const expectedBytes = new TextEncoder().encode(
      `avalon:continuation:v1:id-a:key-b:${token.nonce}:${issuedSecs}:${expiresSecs}`,
    )

    const signature = hexToBytes(token.signature)
    expect(ed25519.verify(signature, expectedBytes, publicKey)).toBe(true)
  })

  it('a signature does not verify against a tampered field (wrong identity_id)', () => {
    const secretKey = ed25519.keygen().secretKey
    const publicKey = ed25519.getPublicKey(secretKey)
    const wire = mintContinuationToken('real-identity', 'key', secretKey)
    const token = decode(wire)

    const issuedSecs = Math.floor(new Date(token.issued_at).getTime() / 1000)
    const expiresSecs = Math.floor(new Date(token.expires_at).getTime() / 1000)
    const tamperedBytes = new TextEncoder().encode(
      `avalon:continuation:v1:different-identity:key:${token.nonce}:${issuedSecs}:${expiresSecs}`,
    )

    const signature = hexToBytes(token.signature)
    expect(ed25519.verify(signature, tamperedBytes, publicKey)).toBe(false)
  })

  it('mints a fresh, distinct nonce on every call (anti-replay)', () => {
    const secretKey = ed25519.keygen().secretKey
    const first = decode(mintContinuationToken('id', 'key', secretKey))
    const second = decode(mintContinuationToken('id', 'key', secretKey))
    expect(first.nonce).not.toBe(second.nonce)
  })
})
