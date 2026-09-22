import { afterEach, describe, expect, it, vi } from 'vitest'
import { AccountSession } from '../../src/accountSession/core.js'
import '../../src/accountSession/social.js'

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

function mockFetchOnce(body: unknown) {
  vi.stubGlobal(
    'fetch',
    vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: () => Promise.resolve(body),
      text: () => Promise.resolve(body === undefined ? '' : JSON.stringify(body)),
    }),
  )
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('AccountSession.identityProfile', () => {
  it('carries mainGuild/effectiveMainGuild through, same as the rest of the profile', async () => {
    mockFetchOnce({
      identity_id: 'id-1',
      identity_created_at: 'now',
      display_name: 'Nova',
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
      effective_main_guild: 'guild-1',
    })
    const result = await testSession().identityProfile('id-1')
    expect(result.mainGuild).toBeNull()
    expect(result.effectiveMainGuild).toBe('guild-1')
  })
})

describe('AccountSession.presenceOf', () => {
  it('passes through the real crates/protocol PresenceStatus wire values unchanged', async () => {
    mockFetchOnce([
      { identity_id: 'id-1', status: 'DoNotDisturb', active_in: null, updated_at: 'now' },
    ])
    const result = await testSession().presenceOf(['id-1'])
    expect(result).toEqual([{ identityId: 'id-1', status: 'DoNotDisturb', activeIn: null, updatedAt: 'now' }])
  })
})

describe('non-array response bodies are passed through, not crashed on', () => {
  it('friendRequests', async () => {
    mockFetchOnce(null)
    expect(await testSession().friendRequests()).toBeNull()
  })

  it('blocks', async () => {
    mockFetchOnce(null)
    expect(await testSession().blocks()).toBeNull()
  })

  it('profiles', async () => {
    mockFetchOnce(null)
    expect(await testSession().profiles(['id-1'])).toBeNull()
  })

  it('discoverPeople', async () => {
    mockFetchOnce(null)
    expect(await testSession().discoverPeople()).toBeNull()
  })

  it('discoverPeople with a malformed (non-array candidates) body', async () => {
    mockFetchOnce({ candidates: null })
    expect(await testSession().discoverPeople()).toEqual({ candidates: null })
  })

  it('searchIdentities', async () => {
    mockFetchOnce(null)
    expect(await testSession().searchIdentities('nova')).toBeNull()
  })
})

describe('AccountSession.discoverPeople / searchIdentities', () => {
  it('unwraps the candidates/results envelope into a flat array', async () => {
    // The real server response only ever carries `identity_id` — see
    // src/accountSession/social.ts's own DiscoveryCandidate doc comment.
    mockFetchOnce({
      candidates: [{ identity_id: 'id-1' }],
    })
    expect(await testSession().discoverPeople()).toEqual([{ identityId: 'id-1' }])

    mockFetchOnce({ results: [{ identity_id: 'id-2', display_name: 'Bramble', avatar_url: null }] })
    expect(await testSession().searchIdentities('bramble')).toEqual([
      { identityId: 'id-2', displayName: 'Bramble', avatarUrl: null },
    ])
  })
})

describe('AccountSession.resolveHandle', () => {
  it('returns the bare identity id string, not a wrapper object', async () => {
    mockFetchOnce({ identity_id: 'id-1' })
    expect(await testSession().resolveHandle('nova')).toBe('id-1')
  })
})
