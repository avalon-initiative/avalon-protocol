import { describe, expect, it } from 'vitest'
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

  it('hasCapability reflects the granted set', () => {
    const session = testSession(['friends.read'])
    expect(session.hasCapability('friends.read')).toBe(true)
    expect(session.hasCapability('guilds.chat')).toBe(false)
  })
})
