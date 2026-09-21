import { describe, expect, it } from 'vitest'
import { generateSigningKey, verify } from './signing.js'
import { mintCrossNodeLoginGrant, DEFAULT_TTL_SECONDS } from './crossNodeLogin.js'

function hexToBytes(hex: string): Uint8Array {
  const bytes = new Uint8Array(hex.length / 2)
  for (let i = 0; i < bytes.length; i++) {
    bytes[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16)
  }
  return bytes
}

describe('mintCrossNodeLoginGrant', () => {
  it('produces a grant with the exact field shape CrossNodeLoginGrant expects', () => {
    const { secretKey } = generateSigningKey()
    const grant = mintCrossNodeLoginGrant(
      'identity-1',
      'key-1',
      secretKey,
      'https://destination.example',
      'console-login',
    )

    expect(grant.identity_id).toBe('identity-1')
    expect(grant.signing_key_id).toBe('key-1')
    expect(grant.destination_base_url).toBe('https://destination.example')
    expect(grant.requesting_context).toBe('console-login')
    expect(typeof grant.nonce).toBe('string')
    expect(new Date(grant.issued_at).getTime()).not.toBeNaN()
    expect(new Date(grant.expires_at).getTime()).not.toBeNaN()
    // Hex, not base64 — the one detail continuation.ts's own signature also
    // gets right and this must match byte-for-byte.
    expect(grant.signature).toMatch(/^[0-9a-f]+$/)
  })

  it('expires exactly 60 seconds after issued_at, matching DEFAULT_TTL_SECONDS', () => {
    const { secretKey } = generateSigningKey()
    const grant = mintCrossNodeLoginGrant('id', 'key', secretKey, 'https://dest.example', 'ctx')
    const issued = new Date(grant.issued_at).getTime()
    const expires = new Date(grant.expires_at).getTime()
    expect(DEFAULT_TTL_SECONDS).toBe(60)
    expect(expires - issued).toBe(60_000)
  })

  it('signs bytes in the exact format crates/server/src/cross_node_login.rs::verify recomputes', () => {
    const { secretKey, publicKey } = generateSigningKey()
    const grant = mintCrossNodeLoginGrant(
      'id-a',
      'key-b',
      secretKey,
      'https://dest.example',
      'requesting-context',
    )

    const issuedSecs = Math.floor(new Date(grant.issued_at).getTime() / 1000)
    const expiresSecs = Math.floor(new Date(grant.expires_at).getTime() / 1000)
    const expectedBytes = new TextEncoder().encode(
      `avalon:cross-node-login:v1:id-a:key-b:https://dest.example:requesting-context:${grant.nonce}:${issuedSecs}:${expiresSecs}`,
    )

    const signature = hexToBytes(grant.signature)
    expect(verify(publicKey, expectedBytes, signature)).toBe(true)
  })

  it('a signature does not verify against a tampered destination_base_url', () => {
    const { secretKey, publicKey } = generateSigningKey()
    const grant = mintCrossNodeLoginGrant('id', 'key', secretKey, 'https://real.example', 'ctx')

    const issuedSecs = Math.floor(new Date(grant.issued_at).getTime() / 1000)
    const expiresSecs = Math.floor(new Date(grant.expires_at).getTime() / 1000)
    const tamperedBytes = new TextEncoder().encode(
      `avalon:cross-node-login:v1:id:key:https://attacker.example:ctx:${grant.nonce}:${issuedSecs}:${expiresSecs}`,
    )

    const signature = hexToBytes(grant.signature)
    expect(verify(publicKey, tamperedBytes, signature)).toBe(false)
  })

  it('mints a fresh, distinct nonce on every call (anti-replay)', () => {
    const { secretKey } = generateSigningKey()
    const first = mintCrossNodeLoginGrant('id', 'key', secretKey, 'https://dest.example', 'ctx')
    const second = mintCrossNodeLoginGrant('id', 'key', secretKey, 'https://dest.example', 'ctx')
    expect(first.nonce).not.toBe(second.nonce)
  })
})
