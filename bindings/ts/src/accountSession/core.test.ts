import { describe, expect, it } from 'vitest'
import { AccountSession } from './core.js'
import { canonicalMessage, generateSigningKey, verify } from '../crypto/signing.js'
import './passkeys.js'

function testIdentity() {
  return { id: crypto.randomUUID(), createdAt: new Date().toISOString() }
}

function testProfile(identityId: string) {
  return {
    identityId,
    displayName: 'test',
    avatarUrl: null,
    bio: null,
    favoriteGenres: [],
    pronouns: null,
    bannerUrl: null,
    status: null,
    links: [],
    timezone: null,
    themeColor: null,
    location: null,
    mainGuild: null,
  }
}

describe('AccountSession.sign', () => {
  it('with no local key yields null signature fields', () => {
    const identity = testIdentity()
    const session = new AccountSession({
      identity,
      profile: testProfile(identity.id),
      serverUrl: 'http://127.0.0.1:1',
      token: 'test-token',
    })
    const signed = session.sign('guild.transfer_ownership', ['g1', 'from1', 'to1'])
    expect(signed.signing_key_id).toBeNull()
    expect(signed.signature).toBeNull()
  })

  it('with a local key produces a verifiable signature', () => {
    const identity = testIdentity()
    const { secretKey, publicKey } = generateSigningKey()
    const signingKeyId = crypto.randomUUID()
    const session = new AccountSession({
      identity,
      profile: testProfile(identity.id),
      serverUrl: 'http://127.0.0.1:1',
      token: 'test-token',
      signing: { secretKey, publicKey, signingKeyId },
    })

    const signed = session.sign('passkey.revoke_last', ['p1', 'i1'])
    expect(signed.signing_key_id).toBe(signingKeyId)
    expect(signed.signature).not.toBeNull()

    const signatureBytes = Uint8Array.from(atob(signed.signature!), (c) => c.charCodeAt(0))
    expect(verify(publicKey, canonicalMessage('passkey.revoke_last', ['p1', 'i1']), signatureBytes)).toBe(true)
  })
})

describe('a signed call round trip', () => {
  it('sends a signing_key_id/signature that verifies server-side-equivalently', async () => {
    const identity = testIdentity()
    const { secretKey, publicKey } = generateSigningKey()
    const signingKeyId = crypto.randomUUID()
    const session = new AccountSession({
      identity,
      profile: testProfile(identity.id),
      serverUrl: 'http://127.0.0.1:1',
      token: 'test-token',
      signing: { secretKey, publicKey, signingKeyId },
    })

    let capturedBody: { signing_key_id: string; signature: string } | undefined
    const originalFetch = globalThis.fetch
    globalThis.fetch = (async (_url: string, init?: RequestInit) => {
      capturedBody = JSON.parse(init!.body as string)
      return new Response('', { status: 200 })
    }) as typeof fetch

    try {
      const passkeyId = crypto.randomUUID()
      await session.revokePasskey(passkeyId)

      expect(capturedBody).toBeDefined()
      expect(capturedBody!.signing_key_id).toBe(signingKeyId)
      const signatureBytes = Uint8Array.from(atob(capturedBody!.signature), (c) => c.charCodeAt(0))
      const expectedMessage = canonicalMessage('passkey.revoke_last', [passkeyId, identity.id])
      expect(verify(publicKey, expectedMessage, signatureBytes)).toBe(true)
    } finally {
      globalThis.fetch = originalFetch
    }
  })
})
