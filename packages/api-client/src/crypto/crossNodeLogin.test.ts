import { describe, expect, it } from 'vitest'
import { ed25519 } from '@noble/curves/ed25519'
import { mintCrossNodeLoginGrant } from './crossNodeLogin'

function hexToBytes(hex: string): Uint8Array {
  const bytes = new Uint8Array(hex.length / 2)
  for (let i = 0; i < bytes.length; i++) {
    bytes[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16)
  }
  return bytes
}

describe('mintCrossNodeLoginGrant', () => {
  it('produces the exact field shape avalon_protocol::cross_node_login::CrossNodeLoginGrant expects', () => {
    const secretKey = ed25519.keygen().secretKey
    const grant = mintCrossNodeLoginGrant(
      '11111111-1111-1111-1111-111111111111',
      'key-42',
      secretKey,
      'https://node-b.example',
      'https://node-b.example',
    )

    expect(grant.identity_id).toBe('11111111-1111-1111-1111-111111111111')
    expect(grant.signing_key_id).toBe('key-42')
    expect(grant.destination_base_url).toBe('https://node-b.example')
    expect(grant.requesting_context).toBe('https://node-b.example')
    expect(typeof grant.nonce).toBe('string')
    expect(new Date(grant.issued_at).getTime()).not.toBeNaN()
    expect(new Date(grant.expires_at).getTime()).not.toBeNaN()
    expect(grant.signature).toMatch(/^[0-9a-f]+$/)
  })

  it('expires exactly 60 seconds after issued_at, matching DEFAULT_TTL_SECONDS', () => {
    const secretKey = ed25519.keygen().secretKey
    const grant = mintCrossNodeLoginGrant('id', 'key', secretKey, 'https://node-a.example', 'ctx')
    const issued = new Date(grant.issued_at).getTime()
    const expires = new Date(grant.expires_at).getTime()
    expect(expires - issued).toBe(60 * 1000)
  })

  it('signs bytes in the exact format avalon_protocol::cross_node_login::signing_bytes recomputes', () => {
    const secretKey = ed25519.keygen().secretKey
    const publicKey = ed25519.getPublicKey(secretKey)
    const grant = mintCrossNodeLoginGrant(
      'id-a',
      'key-b',
      secretKey,
      'https://node-b.example',
      'SomeGame (node-b.example)',
    )

    const issuedSecs = Math.floor(new Date(grant.issued_at).getTime() / 1000)
    const expiresSecs = Math.floor(new Date(grant.expires_at).getTime() / 1000)
    const expectedBytes = new TextEncoder().encode(
      `avalon:cross-node-login:v1:id-a:key-b:https://node-b.example:SomeGame (node-b.example):` +
        `${grant.nonce}:${issuedSecs}:${expiresSecs}`,
    )

    const signature = hexToBytes(grant.signature)
    expect(ed25519.verify(signature, expectedBytes, publicKey)).toBe(true)
  })

  it('a signature does not verify once destination_base_url is swapped for a different one', () => {
    // The whole point of #610's lesson, applied here: destination_base_url
    // is signed, so a grant can't be relayed to a node the human never
    // actually saw approving.
    const secretKey = ed25519.keygen().secretKey
    const publicKey = ed25519.getPublicKey(secretKey)
    const grant = mintCrossNodeLoginGrant(
      'real-identity',
      'key',
      secretKey,
      'https://legitimate-node.example',
      'ctx',
    )

    const issuedSecs = Math.floor(new Date(grant.issued_at).getTime() / 1000)
    const expiresSecs = Math.floor(new Date(grant.expires_at).getTime() / 1000)
    const redirectedBytes = new TextEncoder().encode(
      `avalon:cross-node-login:v1:real-identity:key:https://attacker.example:ctx:` +
        `${grant.nonce}:${issuedSecs}:${expiresSecs}`,
    )

    const signature = hexToBytes(grant.signature)
    expect(ed25519.verify(signature, redirectedBytes, publicKey)).toBe(false)
  })

  it('mints a fresh, distinct nonce on every call', () => {
    const secretKey = ed25519.keygen().secretKey
    const first = mintCrossNodeLoginGrant('id', 'key', secretKey, 'https://node-a.example', 'ctx')
    const second = mintCrossNodeLoginGrant('id', 'key', secretKey, 'https://node-a.example', 'ctx')
    expect(first.nonce).not.toBe(second.nonce)
  })
})
