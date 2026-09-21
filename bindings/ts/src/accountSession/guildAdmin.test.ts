import { describe, expect, it } from 'vitest'
import { AccountSession } from './core.js'
import './guildAdmin.js'

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
    effectiveMainGuild: null,
    discoverable: false,
    presenceVisibility: 'public',
  }
}

function testSession(): AccountSession {
  const identity = testIdentity()
  return new AccountSession({
    identity,
    profile: testProfile(identity.id),
    serverUrl: 'http://127.0.0.1:1',
    token: 'test-token',
  })
}

describe('AccountSession.getGameBreakdown', () => {
  it('converts the wire snake_case breakdown into camelCase', async () => {
    const session = testSession()
    const originalFetch = globalThis.fetch
    globalThis.fetch = (async () =>
      new Response(
        JSON.stringify({
          guild_id: 'g1',
          total_members: 5,
          breakdown: [
            { integrator_id: 'i1', integrator_slug: 'some-game', integrator_name: 'Some Game', member_count: 3 },
          ],
        }),
        { status: 200 },
      )) as typeof fetch

    try {
      const result = await session.getGameBreakdown('g1')
      expect(result).toEqual({
        guildId: 'g1',
        totalMembers: 5,
        breakdown: [{ integratorId: 'i1', integratorSlug: 'some-game', integratorName: 'Some Game', memberCount: 3 }],
      })
    } finally {
      globalThis.fetch = originalFetch
    }
  })
})

describe('AccountSession.getMessageArchive', () => {
  it('converts archived-message wire shapes into camelCase and applies before/limit query params', async () => {
    const session = testSession()
    let capturedUrl: string | undefined
    const originalFetch = globalThis.fetch
    globalThis.fetch = (async (url: string) => {
      capturedUrl = url
      return new Response(
        JSON.stringify([
          {
            id: 'm1',
            channel_id: 'c1',
            author: 'a1',
            body: 'hello',
            sent_at: '2026-01-01T00:00:00Z',
            archived_at: '2026-02-01T00:00:00Z',
          },
        ]),
        { status: 200 },
      )
    }) as typeof fetch

    try {
      const result = await session.getMessageArchive('g1', 'c1', 'cursor-1', 25)
      expect(result).toEqual([
        {
          id: 'm1',
          channelId: 'c1',
          author: 'a1',
          body: 'hello',
          sentAt: '2026-01-01T00:00:00Z',
          archivedAt: '2026-02-01T00:00:00Z',
        },
      ])
      expect(capturedUrl).toContain('/guilds/g1/channels/c1/messages/archive')
      expect(capturedUrl).toContain('before=cursor-1')
      expect(capturedUrl).toContain('limit=25')
    } finally {
      globalThis.fetch = originalFetch
    }
  })
})
