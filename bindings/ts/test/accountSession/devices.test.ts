import { afterEach, describe, expect, it, vi } from 'vitest'
import { AccountSession } from '../../src/accountSession/core.js'
import { bytesToBase64, generateSigningKey } from '../../src/crypto/signing.js'
import { NoLocalSigningKeyError } from '../../src/errors.js'
import { ed25519 } from '@noble/curves/ed25519'
import '../../src/accountSession/devices.js'

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
    effectiveMainGuild: null,
    discoverable: false,
    presenceVisibility: 'public',
  }
}

function testSession(signing?: { secretKey: Uint8Array; publicKey: Uint8Array; signingKeyId: string }): AccountSession {
  const identity = { id: crypto.randomUUID(), createdAt: new Date().toISOString() }
  return new AccountSession({
    identity,
    profile: testProfile(identity.id),
    serverUrl: 'http://127.0.0.1:1',
    token: 'test-token',
    signing,
  })
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('AccountSession.approveDeviceGrant', () => {
  it('throws NoLocalSigningKeyError when this session holds no local key', async () => {
    const session = testSession()
    await expect(session.approveDeviceGrant('grant-1', 'requested-key')).rejects.toBeInstanceOf(
      NoLocalSigningKeyError,
    )
  })

  it('signs the exact grant being approved with the approving key', async () => {
    const { secretKey, publicKey } = generateSigningKey()
    const requested = generateSigningKey()
    const session = testSession({ secretKey, publicKey, signingKeyId: 'approver-key-id' })

    let capturedBody: Record<string, unknown> | undefined
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(async (_url: string, init?: RequestInit) => {
        capturedBody = JSON.parse(init!.body as string)
        return new Response(
          JSON.stringify({ id: 'new-device', label: null, public_key: 'x', added_at: 'now' }),
          { status: 200 },
        )
      }),
    )

    const grantId = crypto.randomUUID()
    const requestedPublicKeyB64 = bytesToBase64(requested.publicKey)
    await session.approveDeviceGrant(grantId, requestedPublicKeyB64)

    expect(capturedBody!.approver_signing_key_id).toBe('approver-key-id')

    // The signature must verify against the exact bytes
    // crates/server/src/devices.rs independently reconstructs.
    const signingBytes = new TextEncoder().encode(
      `avalon:device_grant.approved:v1:${grantId}:${session.identity().id}:${requestedPublicKeyB64}`,
    )
    const signature = Uint8Array.from(atob(capturedBody!.signature as string), (c) => c.charCodeAt(0))
    expect(ed25519.verify(signature, signingBytes, publicKey)).toBe(true)
  })
})
