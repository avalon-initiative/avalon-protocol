import { afterEach, describe, expect, it } from 'vitest'
import { IntegratorSession, type Capability } from '../src/integratorSession.js'
import { CapabilityNotGrantedError, MissingIssuerCredentialsError } from '../src/errors.js'

function testSession(granted: Capability[] = []): IntegratorSession {
  const id = crypto.randomUUID()
  return new IntegratorSession({
    identity: { id, createdAt: new Date().toISOString() },
    profile: {
      identityId: id,
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
    granted,
    serverUrl: 'http://127.0.0.1:1',
    token: 'test-token',
    integratorKeyId: 'test-integrator-key',
  })
}

describe('IntegratorSession capability gating', () => {
  it('throws CapabilityNotGrantedError without making a request when ungranted', async () => {
    const session = testSession([])
    await expect(session.friends()).rejects.toBeInstanceOf(CapabilityNotGrantedError)
  })

  it('has no shared base class or conversion with AccountSession', () => {
    // #696's hard invariant, checked structurally: IntegratorSession has no
    // constructor path or static factory that accepts AccountSession's
    // credential shape, and vice versa (see accountSession/core.test.ts).
    expect((IntegratorSession as unknown as { fromAccountSession?: unknown }).fromAccountSession).toBeUndefined()
  })

  it('issueAchievement throws MissingIssuerCredentialsError with no HTTP call when unconfigured', async () => {
    const session = testSession(['achievements.issue'])
    await expect(session.issueAchievement('some-key')).rejects.toBeInstanceOf(MissingIssuerCredentialsError)
  })

  it('issueMilestone throws MissingIssuerCredentialsError with no HTTP call when unconfigured', async () => {
    const session = testSession(['milestones.issue'])
    await expect(session.issueMilestone('some-key')).rejects.toBeInstanceOf(MissingIssuerCredentialsError)
  })

  it('issueMilestone throws CapabilityNotGrantedError without making a request when ungranted', async () => {
    const session = testSession([])
    await expect(session.issueMilestone('some-key')).rejects.toBeInstanceOf(CapabilityNotGrantedError)
  })

  it('bulkIssueAchievements throws MissingIssuerCredentialsError with no HTTP call when unconfigured', async () => {
    const session = testSession(['achievements.issue'])
    await expect(session.bulkIssueAchievements([{ key: 'a' }])).rejects.toBeInstanceOf(MissingIssuerCredentialsError)
  })

  it('bulkIssueMilestones throws CapabilityNotGrantedError without making a request when ungranted', async () => {
    const session = testSession([])
    await expect(session.bulkIssueMilestones([{ key: 'a' }])).rejects.toBeInstanceOf(CapabilityNotGrantedError)
  })

  it('updateIntegratorPresence throws CapabilityNotGrantedError without making a request when ungranted', async () => {
    const session = testSession([])
    await expect(session.updateIntegratorPresence('Online')).rejects.toBeInstanceOf(CapabilityNotGrantedError)
  })

  it('updateIntegratorPresence throws MissingIssuerCredentialsError with no HTTP call when unconfigured', async () => {
    const session = testSession(['presence.publish'])
    await expect(session.updateIntegratorPresence('Online')).rejects.toBeInstanceOf(MissingIssuerCredentialsError)
  })

  it('hasCapability reflects the granted set', () => {
    const session = testSession(['friends.read'])
    expect(session.hasCapability('friends.read')).toBe(true)
    expect(session.hasCapability('guilds.chat')).toBe(false)
  })
})

describe('IntegratorSession.myGrants', () => {
  const originalFetch = globalThis.fetch
  afterEach(() => {
    globalThis.fetch = originalFetch
  })

  it('converts wire snake_case into camelCase and sends the configured key id header', async () => {
    let capturedHeaders: Record<string, string> | undefined
    globalThis.fetch = (async (_url, init) => {
      capturedHeaders = init?.headers as Record<string, string>
      return new Response(JSON.stringify({ integrator_id: 'i1', capabilities: ['achievements.issue'] }), { status: 200 })
    }) as typeof fetch

    const session = testSession([])
    const grants = await session.myGrants()
    expect(grants).toEqual({ integratorId: 'i1', capabilities: ['achievements.issue'] })
    expect(capturedHeaders?.['x-avalon-integrator-key-id']).toBe('test-integrator-key')
  })
})
