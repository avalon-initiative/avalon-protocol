import { afterEach, describe, expect, it, vi } from 'vitest'
import { AccountSession } from './core.js'
import './integrations.js'

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

function testSession(): AccountSession {
  const identity = { id: crypto.randomUUID(), createdAt: new Date().toISOString() }
  return new AccountSession({
    identity,
    profile: testProfile(identity.id),
    serverUrl: 'http://127.0.0.1:1',
    token: 'test-token',
  })
}

afterEach(() => {
  vi.unstubAllGlobals()
})

function mockFetchOnce(body: unknown) {
  vi.stubGlobal(
    'fetch',
    vi.fn().mockResolvedValue(new Response(body === undefined ? '' : JSON.stringify(body), { status: 200 })),
  )
}

describe('AccountSession.myConnections', () => {
  it('converts wire snake_case into camelCase', async () => {
    mockFetchOnce([
      {
        binding_id: 'b1',
        integrator_id: 'i1',
        slug: 's1',
        name: 'One',
        established_at: 'now',
        grants: [{ capability: 'profile.read', granted_at: 'now' }],
      },
    ])
    const result = await testSession().myConnections()
    expect(result).toEqual([
      {
        bindingId: 'b1',
        integratorId: 'i1',
        slug: 's1',
        name: 'One',
        establishedAt: 'now',
        grants: [{ capability: 'profile.read', grantedAt: 'now' }],
      },
    ])
  })

  it('passes through a non-array body instead of crashing on it', async () => {
    mockFetchOnce(null)
    expect(await testSession().myConnections()).toBeNull()
  })
})

describe('AccountSession.connectIntegrator', () => {
  it('signs and sends the requested capabilities', async () => {
    const session = testSession()
    let capturedBody: Record<string, unknown> | undefined
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(async (_url: string, init?: RequestInit) => {
        capturedBody = JSON.parse(init!.body as string)
        return new Response(
          JSON.stringify({
            binding_id: 'b1',
            integrator_id: 'i1',
            established_at: 'now',
            granted_capabilities: ['profile.read'],
          }),
          { status: 200 },
        )
      }),
    )
    const result = await session.connectIntegrator('s1', ['profile.read'])
    expect(capturedBody!.capabilities).toEqual(['profile.read'])
    expect(result).toEqual({
      bindingId: 'b1',
      integratorId: 'i1',
      establishedAt: 'now',
      grantedCapabilities: ['profile.read'],
    })
  })
})
