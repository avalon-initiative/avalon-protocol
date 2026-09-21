import { describe, expect, it } from 'vitest'
import { AccountSession } from '../../src/accountSession/core.js'
import '../../src/accountSession/guildAdmin.js'

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

describe('AccountSession.getGuild', () => {
  it('carries memberCount/integrators/gameBreakdownPublic/favoriteGames/rosterVisibility through from the wire', async () => {
    const session = testSession()
    const originalFetch = globalThis.fetch
    globalThis.fetch = (async () =>
      new Response(
        JSON.stringify({
          id: 'g1',
          name: 'Dragon Hunters',
          tag: 'DRGN',
          description: 'A guild.',
          owner: 'id-owner',
          created_at: 'now',
          member_count: 42,
          integrators: ['i1'],
          join_policy: 'invite_only',
          motd: null,
          banner: null,
          icon: null,
          links: [],
          recruiting: false,
          public: true,
          game_breakdown_public: true,
          favorite_games: [
            { integrator_id: 'i1', integrator_slug: 'some-game', integrator_name: 'Some Game', position: 0, stale: false },
          ],
          roster_visibility: 'public',
        }),
        { status: 200 },
      )) as typeof fetch

    try {
      const guild = await session.getGuild('g1')
      expect(guild.memberCount).toBe(42)
      expect(guild.integrators).toEqual(['i1'])
      expect(guild.gameBreakdownPublic).toBe(true)
      expect(guild.favoriteGames).toEqual([
        { integratorId: 'i1', integratorSlug: 'some-game', integratorName: 'Some Game', position: 0, stale: false },
      ])
      expect(guild.rosterVisibility).toBe('public')
    } finally {
      globalThis.fetch = originalFetch
    }
  })
})

describe('AccountSession.updateGuild', () => {
  it('sends joinPolicy/gameBreakdownPublic/rosterVisibility/links as their snake_case wire names', async () => {
    const session = testSession()
    let capturedBody: Record<string, unknown> | undefined
    const originalFetch = globalThis.fetch
    globalThis.fetch = (async (_url: string, init?: RequestInit) => {
      capturedBody = JSON.parse(init!.body as string)
      return new Response(
        JSON.stringify({
          id: 'g1',
          name: 'Dragon Hunters',
          tag: 'DRGN',
          description: 'A guild.',
          owner: 'id-owner',
          created_at: 'now',
          member_count: 1,
          integrators: [],
          join_policy: 'open',
          motd: null,
          banner: null,
          icon: null,
          links: [{ label: 'Website', url: 'https://example.com' }],
          recruiting: false,
          public: false,
          game_breakdown_public: true,
          favorite_games: [],
          roster_visibility: 'private',
        }),
        { status: 200 },
      )
    }) as typeof fetch

    try {
      const guild = await session.updateGuild('g1', {
        joinPolicy: 'open',
        gameBreakdownPublic: true,
        rosterVisibility: 'private',
        links: [{ label: 'Website', url: 'https://example.com' }],
      })
      expect(capturedBody).toMatchObject({
        join_policy: 'open',
        game_breakdown_public: true,
        roster_visibility: 'private',
        links: [{ label: 'Website', url: 'https://example.com' }],
      })
      expect(guild.joinPolicy).toBe('open')
      expect(guild.rosterVisibility).toBe('private')
    } finally {
      globalThis.fetch = originalFetch
    }
  })
})

describe('AccountSession.createRole / updateRole badge', () => {
  it('sends badge through on create and update', async () => {
    const session = testSession()
    let capturedBody: Record<string, unknown> | undefined
    const originalFetch = globalThis.fetch
    globalThis.fetch = (async (_url: string, init?: RequestInit) => {
      capturedBody = JSON.parse(init!.body as string)
      return new Response(
        JSON.stringify({
          name_index: 1,
          name: 'Officer',
          permissions: [],
          description: '',
          badge: { icon: 'star', color: 'gold' },
        }),
        { status: 200 },
      )
    }) as typeof fetch

    try {
      const role = await session.createRole('g1', 'Officer', [], '', { icon: 'star', color: 'gold' })
      expect(capturedBody?.badge).toEqual({ icon: 'star', color: 'gold' })
      expect(role.badge).toEqual({ icon: 'star', color: 'gold' })

      await session.updateRole('g1', 1, undefined, undefined, undefined, { icon: 'star', color: 'gold' })
      expect(capturedBody?.badge).toEqual({ icon: 'star', color: 'gold' })
    } finally {
      globalThis.fetch = originalFetch
    }
  })
})

describe('AccountSession.getEvent (myRsvp/detailsVisible via listEvents)', () => {
  it('carries myRsvp/detailsVisible through from the wire', async () => {
    const session = testSession()
    const originalFetch = globalThis.fetch
    globalThis.fetch = (async () =>
      new Response(
        JSON.stringify([
          {
            id: 'e1',
            guild_id: 'g1',
            channel_id: null,
            title: 'Raid night',
            description: null,
            starts_at: '2026-01-01T00:00:00Z',
            ends_at: null,
            created_by: 'id-1',
            created_at: '2026-01-01T00:00:00Z',
            rsvp_counts: { going: 1, maybe: 0, not_going: 0 },
            public: false,
            my_rsvp: 'going',
            details_visible: true,
          },
        ]),
        { status: 200 },
      )) as typeof fetch

    try {
      const [event] = await session.listEvents('g1')
      expect(event.myRsvp).toBe('going')
      expect(event.detailsVisible).toBe(true)
    } finally {
      globalThis.fetch = originalFetch
    }
  })
})

describe('non-array response bodies are passed through, not crashed on', () => {
  it('listMembers/myGuilds/listChannels/listEvents/eventRsvps/listJoinRequests/myGuildInvites/listRoles/getMessageArchive/listPermissionOverrides', async () => {
    const session = testSession()
    const originalFetch = globalThis.fetch
    globalThis.fetch = (async () => new Response(JSON.stringify(null), { status: 200 })) as typeof fetch

    try {
      expect(await session.listMembers('g1')).toBeNull()
      expect(await session.myGuilds()).toBeNull()
      expect(await session.listChannels('g1')).toBeNull()
      expect(await session.listEvents('g1')).toBeNull()
      expect(await session.eventRsvps('g1', 'e1')).toBeNull()
      expect(await session.listJoinRequests('g1')).toBeNull()
      expect(await session.myGuildInvites()).toBeNull()
      expect(await session.listRoles('g1')).toBeNull()
      expect(await session.getMessageArchive('g1', 'c1')).toBeNull()
      expect(await session.listPermissionOverrides('g1', 'channel', 'c1')).toBeNull()
    } finally {
      globalThis.fetch = originalFetch
    }
  })

  it('discoverGuilds passes through a body with no guilds array', async () => {
    const session = testSession()
    const originalFetch = globalThis.fetch
    globalThis.fetch = (async () =>
      new Response(JSON.stringify({ guilds: null }), { status: 200 })) as typeof fetch

    try {
      expect(await session.discoverGuilds('')).toEqual({ guilds: null })
    } finally {
      globalThis.fetch = originalFetch
    }
  })
})
