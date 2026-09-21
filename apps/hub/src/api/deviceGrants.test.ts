import { describe, expect, it, vi, afterEach } from 'vitest'
import { AccountSession } from '@avalon/sdk'
import { beginDeviceGrantRequest } from './deviceGrants'

function testSession(): AccountSession {
  const identityId = crypto.randomUUID()
  return new AccountSession({
    identity: { id: identityId, createdAt: 'now' },
    profile: {
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
    },
    serverUrl: 'http://test',
    token: 'token',
  })
}

function mockFetchOnce(body: unknown) {
  vi.stubGlobal(
    'fetch',
    vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: () => Promise.resolve(body),
      text: () => Promise.resolve(JSON.stringify(body)),
    }),
  )
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('beginDeviceGrantRequest', () => {
  it('generates a keypair, requests a grant, and never persists the key itself', async () => {
    mockFetchOnce({
      id: 'grant-1',
      status: 'pending',
      device_label: null,
      requested_signing_public_key: 'placeholder',
      requested_at: 'now',
      expires_at: 'later',
    })

    const { grant, secretKey } = await beginDeviceGrantRequest(testSession())

    expect(grant.id).toBe('grant-1')
    expect(secretKey).toHaveLength(32)

    const [, options] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    const sentBody = JSON.parse(options.body)
    expect(typeof sentBody.requested_signing_public_key).toBe('string')
  })
})
