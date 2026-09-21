import { describe, expect, it } from 'vitest'
import { AccountSession } from '../../src/accountSession/core.js'
import { canonicalMessage, generateSigningKey, verify } from '../../src/crypto/signing.js'
import { WIRE_PREFIX } from '../../src/crypto/continuation.js'
import '../../src/accountSession/passkeys.js'

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

describe('AccountSession 401 continuation-reconnect (issue #525)', () => {
  it('mints a continuation token from its own in-memory signing key and retries once', async () => {
    const identity = testIdentity()
    const { secretKey, publicKey } = generateSigningKey()
    const signingKeyId = crypto.randomUUID()
    const session = new AccountSession({
      identity,
      profile: testProfile(identity.id),
      serverUrl: 'http://127.0.0.1:1',
      token: 'stale-token',
      signing: { secretKey, publicKey, signingKeyId },
    })

    let calls = 0
    const originalFetch = globalThis.fetch
    globalThis.fetch = (async (_url: string, init?: RequestInit) => {
      calls += 1
      const auth = (init?.headers as Record<string, string>)?.authorization
      if (calls === 1) {
        expect(auth).toBe('Bearer stale-token')
        return new Response('', { status: 401 })
      }
      expect(auth?.startsWith(`Bearer ${WIRE_PREFIX}`)).toBe(true)
      return new Response(
        JSON.stringify({
          identity_id: identity.id,
          identity_created_at: identity.createdAt,
          display_name: 'test',
          avatar_url: null,
          bio: null,
          favorite_genres: [],
          pronouns: null,
          banner_url: null,
          status: null,
          links: [],
          timezone: null,
          theme_color: null,
          location: null,
          main_guild: null,
        }),
        { status: 200 },
      )
    }) as typeof fetch

    try {
      await session.refreshProfile()
      expect(calls).toBe(2)
      // Never persisted back into the session's own token — single-use.
      expect(session.token()).toBe('stale-token')
    } finally {
      globalThis.fetch = originalFetch
    }
  })

  it('propagates the 401 as-is when this session holds no local signing key', async () => {
    const identity = testIdentity()
    const session = new AccountSession({
      identity,
      profile: testProfile(identity.id),
      serverUrl: 'http://127.0.0.1:1',
      token: 'stale-token',
    })

    let calls = 0
    const originalFetch = globalThis.fetch
    globalThis.fetch = (async () => {
      calls += 1
      return new Response('', { status: 401 })
    }) as typeof fetch

    try {
      await expect(session.refreshProfile()).rejects.toThrow()
      expect(calls).toBe(1)
    } finally {
      globalThis.fetch = originalFetch
    }
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

describe('AccountSession.attachSigningKey', () => {
  it('resolves the matching signing_key_id and enables auto-signing', async () => {
    const identity = testIdentity()
    const { secretKey, publicKey } = generateSigningKey()
    const publicKeyB64 = btoa(String.fromCharCode(...publicKey))
    const signingKeyId = crypto.randomUUID()
    const session = new AccountSession({
      identity,
      profile: testProfile(identity.id),
      serverUrl: 'http://127.0.0.1:1',
      token: 'test-token',
    })

    const originalFetch = globalThis.fetch
    globalThis.fetch = (async () =>
      new Response(
        JSON.stringify([
          { id: crypto.randomUUID(), public_key: 'some-other-key' },
          { id: signingKeyId, public_key: publicKeyB64 },
        ]),
        { status: 200 },
      )) as typeof fetch

    try {
      expect(session.signingKeyId()).toBeUndefined()
      const attached = await session.attachSigningKey(secretKey)
      expect(attached).toBe(true)
      expect(session.signingKeyId()).toBe(signingKeyId)

      const signed = session.sign('guild.transfer_ownership', ['g1', 'from1', 'to1'])
      expect(signed.signing_key_id).toBe(signingKeyId)
    } finally {
      globalThis.fetch = originalFetch
    }
  })

  it('returns false and leaves signing state unchanged when the key was never registered server-side', async () => {
    const identity = testIdentity()
    const { secretKey } = generateSigningKey()
    const session = new AccountSession({
      identity,
      profile: testProfile(identity.id),
      serverUrl: 'http://127.0.0.1:1',
      token: 'test-token',
    })

    const originalFetch = globalThis.fetch
    globalThis.fetch = (async () => new Response(JSON.stringify([]), { status: 200 })) as typeof fetch

    try {
      const attached = await session.attachSigningKey(secretKey)
      expect(attached).toBe(false)
      expect(session.signingKeyId()).toBeUndefined()
    } finally {
      globalThis.fetch = originalFetch
    }
  })
})
