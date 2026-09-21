import { describe, expect, it } from 'vitest'
import { AccountSession } from '../../src/accountSession/core.js'
import '../../src/accountSession/achievements.js'

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

describe('AccountSession.getMyAchievements', () => {
  it('converts the ListMyAchievementsResponse envelope into a flat, camelCase array', async () => {
    const identity = testIdentity()
    const session = new AccountSession({
      identity,
      profile: testProfile(identity.id),
      serverUrl: 'http://127.0.0.1:1',
      token: 'test-token',
    })

    let capturedUrl: string | undefined
    const originalFetch = globalThis.fetch
    globalThis.fetch = (async (url: string) => {
      capturedUrl = url
      return new Response(
        JSON.stringify({
          achievements: [
            {
              id: 'a1',
              issuer: 'game:some-game',
              subject: identity.id,
              achievement: 'game:some-game:achievement:first-blood',
              issued_at: '2026-01-01T00:00:00Z',
              proof: { key_id: 'k1', algorithm: 'ed25519' },
              authenticity: { status: 'authentic', key_id: 'k1' },
              validity: { status: 'valid' },
              history: [{ event: 'issued', at: '2026-01-01T00:00:00Z' }],
            },
          ],
          next_cursor: null,
        }),
        { status: 200 },
      )
    }) as typeof fetch

    try {
      const result = await session.getMyAchievements()
      expect(capturedUrl).toContain('/me/achievements?limit=200')
      expect(result).toEqual([
        {
          id: 'a1',
          issuer: 'game:some-game',
          subject: identity.id,
          achievement: 'game:some-game:achievement:first-blood',
          issuedAt: '2026-01-01T00:00:00Z',
          proof: { keyId: 'k1', algorithm: 'ed25519' },
          authenticity: { status: 'authentic', keyId: 'k1', reason: undefined },
          validity: { status: 'valid', reason: undefined },
          history: [{ event: 'issued', at: '2026-01-01T00:00:00Z', reasonCode: undefined, reason: undefined }],
        },
      ])
    } finally {
      globalThis.fetch = originalFetch
    }
  })

  it('returns an empty array for an identity with no achievements yet', async () => {
    const identity = testIdentity()
    const session = new AccountSession({
      identity,
      profile: testProfile(identity.id),
      serverUrl: 'http://127.0.0.1:1',
      token: 'test-token',
    })

    const originalFetch = globalThis.fetch
    globalThis.fetch = (async () =>
      new Response(JSON.stringify({ achievements: [], next_cursor: null }), { status: 200 })) as typeof fetch

    try {
      const result = await session.getMyAchievements()
      expect(result).toEqual([])
    } finally {
      globalThis.fetch = originalFetch
    }
  })
})
