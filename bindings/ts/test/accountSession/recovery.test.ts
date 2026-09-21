import { afterEach, describe, expect, it, vi } from 'vitest'
import { AccountSession } from '../../src/accountSession/core.js'
import { generateSigningKey } from '../../src/crypto/signing.js'
import '../../src/accountSession/recovery.js'

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

function mockFetchOnce(body: unknown) {
  return vi.fn().mockImplementation(async (_url: string, init?: RequestInit) => ({
    ok: true,
    status: 200,
    json: () => Promise.resolve(body),
    text: () => Promise.resolve(JSON.stringify(body)),
    __init: init,
  }))
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('AccountSession.setGuardians', () => {
  it('sends no signature when this session holds no local key, and returns camelCase fields', async () => {
    const session = testSession()
    const fetchMock = mockFetchOnce({ guardian_ids: ['g2', 'g1'], threshold: 2, updated_at: 'now' })
    vi.stubGlobal('fetch', fetchMock)

    const result = await session.setGuardians(['g1', 'g2'], 2)

    expect(result).toEqual({ guardianIds: ['g2', 'g1'], threshold: 2, updatedAt: 'now' })
    const [, init] = fetchMock.mock.calls[0]
    const sentBody = JSON.parse(init.body)
    expect(sentBody).toEqual({ guardian_ids: ['g1', 'g2'], threshold: 2, signing_key_id: null, signature: null })
  })

  it('signs recovery.guardians.set with the sorted, comma-joined guardian ids when a local key exists', async () => {
    const { secretKey, publicKey } = generateSigningKey()
    const session = testSession({ secretKey, publicKey, signingKeyId: 'key-1' })
    const fetchMock = mockFetchOnce({ guardian_ids: ['g1', 'g2'], threshold: 2, updated_at: 'now' })
    vi.stubGlobal('fetch', fetchMock)

    await session.setGuardians(['g2', 'g1'], 2)

    const [, init] = fetchMock.mock.calls[0]
    const sentBody = JSON.parse(init.body)
    expect(sentBody.signing_key_id).toBe('key-1')
    expect(typeof sentBody.signature).toBe('string')
  })
})

describe('AccountSession.guardianRequests / guardianOf', () => {
  it('renames already_approved to alreadyApproved and identity_id/display_name/added_at to camelCase', async () => {
    const session = testSession()
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(async (url: string) => {
        const body = String(url).includes('guardian-requests')
          ? [
              {
                request: {
                  id: 'r1',
                  identity_id: 'friend-1',
                  status: 'pending_approvals',
                  threshold: 2,
                  approvals_count: 1,
                  requested_at: 'now',
                  delay_ends_at: null,
                },
                already_approved: true,
              },
            ]
          : [{ identity_id: 'owner-1', display_name: 'Bramble', added_at: 'now' }]
        return { ok: true, status: 200, json: () => Promise.resolve(body), text: () => Promise.resolve(JSON.stringify(body)) }
      }),
    )

    const requests = await session.guardianRequests()
    expect(requests).toEqual([
      {
        request: {
          id: 'r1',
          identityId: 'friend-1',
          status: 'pending_approvals',
          threshold: 2,
          approvalsCount: 1,
          requestedAt: 'now',
          delayEndsAt: null,
        },
        alreadyApproved: true,
      },
    ])

    const guardianOf = await session.guardianOf()
    expect(guardianOf).toEqual([{ identityId: 'owner-1', displayName: 'Bramble', addedAt: 'now' }])
  })
})

describe('AccountSession.guardianRequests / guardianOf non-array bodies', () => {
  it('passes through a non-array body instead of crashing on it, so callers can detect it themselves', async () => {
    const session = testSession()
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({ ok: true, status: 200, json: () => Promise.resolve(null), text: () => Promise.resolve('null') }),
    )
    expect(await session.guardianRequests()).toBeNull()
    expect(await session.guardianOf()).toBeNull()
  })
})

describe('AccountSession.myRecoveryStatus', () => {
  it('returns null when nothing is in progress', async () => {
    const session = testSession()
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({ ok: true, status: 200, json: () => Promise.resolve(null), text: () => Promise.resolve('null') }),
    )
    expect(await session.myRecoveryStatus()).toBeNull()
  })
})
