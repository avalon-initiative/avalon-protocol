// Security-critical: this is the code that decides whether the Hub treats a
// server as the real, pinned Avalon network or flags it. Every case here
// signs a real Ed25519 keypair through the exact same `signingMessage` this
// module verifies against — not a mock — so a bug in byte layout would show
// up as a failing "valid signature" case, not just a trivially-true one.
import { ed25519 } from '@noble/curves/ed25519'
import { bytesToHex } from '@noble/hashes/utils'
import { describe, expect, it } from 'vitest'
import type { SignedTreeHeadResponse } from '@avalon/api-client'
import { signingMessage } from './sthMessage'
import { evaluateNetworkTrust, verifyTreeHead } from './verifyNetwork'
import type { TrustAnchorEntry } from './trustAnchors'

function sthFixture(overrides: Partial<SignedTreeHeadResponse> = {}): Omit<SignedTreeHeadResponse, 'signature'> {
  return {
    tree_size: 42,
    root_hash: 'ab'.repeat(32),
    network_id: 'avalon-test-network',
    signing_key_id: 'settlement-operator-1',
    created_at: '2026-09-09T12:00:00Z',
    ...overrides,
  }
}

function signSth(
  secretKey: Uint8Array,
  unsigned: Omit<SignedTreeHeadResponse, 'signature'>,
): SignedTreeHeadResponse {
  const withoutSignature = { ...unsigned, signature: '' }
  const signature = ed25519.sign(signingMessage(withoutSignature), secretKey)
  return { ...unsigned, signature: bytesToHex(signature) }
}

function anchorFor(networkId: string, verifyKey: Uint8Array): TrustAnchorEntry {
  return {
    label: networkId,
    network_id: networkId,
    verify_key: bytesToHex(verifyKey),
    signing_key_id: 'settlement-operator-1',
    environment: 'local-dev',
  }
}

describe('verifyTreeHead', () => {
  it('accepts a genuine signature from the pinned key', () => {
    const { secretKey, publicKey } = ed25519.keygen()
    const sth = signSth(secretKey, sthFixture())

    expect(verifyTreeHead(bytesToHex(publicKey), sth)).toBe(true)
  })

  it('rejects a signature produced by a different key', () => {
    const real = ed25519.keygen()
    const impostor = ed25519.keygen()
    const sth = signSth(impostor.secretKey, sthFixture())

    expect(verifyTreeHead(bytesToHex(real.publicKey), sth)).toBe(false)
  })

  it('rejects a tampered field even with an otherwise-genuine signature', () => {
    const { secretKey, publicKey } = ed25519.keygen()
    const sth = signSth(secretKey, sthFixture())

    expect(verifyTreeHead(bytesToHex(publicKey), { ...sth, tree_size: sth.tree_size + 1 })).toBe(false)
    expect(verifyTreeHead(bytesToHex(publicKey), { ...sth, root_hash: 'cd'.repeat(32) })).toBe(false)
    expect(verifyTreeHead(bytesToHex(publicKey), { ...sth, network_id: 'avalon-mainnet-1' })).toBe(false)
  })

  it('fails closed on malformed signature or key material rather than throwing', () => {
    const { secretKey, publicKey } = ed25519.keygen()
    const sth = signSth(secretKey, sthFixture())

    expect(() => verifyTreeHead('not-hex', sth)).not.toThrow()
    expect(verifyTreeHead('not-hex', sth)).toBe(false)
    expect(verifyTreeHead(bytesToHex(publicKey), { ...sth, signature: 'ab' })).toBe(false)
    expect(verifyTreeHead('ab', sth)).toBe(false)
    expect(verifyTreeHead(bytesToHex(publicKey), { ...sth, created_at: 'not-a-date' })).toBe(false)
  })
})

describe('evaluateNetworkTrust', () => {
  it('reports verified when the claimed network is pinned and the signature checks out', () => {
    const { secretKey, publicKey } = ed25519.keygen()
    const sth = signSth(secretKey, sthFixture())
    const anchors = [anchorFor('avalon-test-network', publicKey)]

    const result = evaluateNetworkTrust(anchors, sth)

    expect(result.kind).toBe('verified')
  })

  it('flags a forged STH as a mismatch rather than silently trusting it', () => {
    const real = ed25519.keygen()
    const impostor = ed25519.keygen()
    // An impostor server serving the exact same network_id string, signed
    // with a key that is not the one pinned for that network.
    const forgedSth = signSth(impostor.secretKey, sthFixture())
    const anchors = [anchorFor('avalon-test-network', real.publicKey)]

    const result = evaluateNetworkTrust(anchors, forgedSth)

    expect(result.kind).toBe('mismatch')
    if (result.kind === 'mismatch') {
      expect(result.claimedNetworkId).toBe('avalon-test-network')
    }
  })

  it('reports unknown-network for a network_id outside the pinned list, never treating it as trusted', () => {
    const { secretKey } = ed25519.keygen()
    const sth = signSth(secretKey, sthFixture({ network_id: 'some-random-fork' }))

    const result = evaluateNetworkTrust([], sth)

    expect(result.kind).toBe('unknown-network')
    if (result.kind === 'unknown-network') {
      expect(result.claimedNetworkId).toBe('some-random-fork')
    }
  })
})
