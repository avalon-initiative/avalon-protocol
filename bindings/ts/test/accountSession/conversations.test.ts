import { afterEach, describe, expect, it, vi } from 'vitest'
import { AccountSession } from '../../src/accountSession/core.js'
import '../../src/accountSession/conversations.js'

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
      text: () => Promise.resolve(body === undefined ? '' : JSON.stringify(body)),
    }),
  )
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('AccountSession.conversationMessages', () => {
  it('converts wire snake_case into camelCase', async () => {
    mockFetchOnce([{ id: 'm1', conversation_id: 'c1', author: 'a1', body: 'hi', sent_at: 'now' }])
    const result = await testSession().conversationMessages('c1')
    expect(result).toEqual([{ id: 'm1', conversationId: 'c1', author: 'a1', body: 'hi', sentAt: 'now' }])
  })

  it('passes through a non-array body instead of crashing on it', async () => {
    mockFetchOnce(null)
    expect(await testSession().conversationMessages('c1')).toBeNull()
  })
})

describe('AccountSession.listConversations', () => {
  it('passes through a non-array body instead of crashing on it', async () => {
    mockFetchOnce(null)
    expect(await testSession().listConversations()).toBeNull()
  })
})

describe('AccountSession.createConversation / sendConversationMessage', () => {
  it('sends participants/body as the request payload', async () => {
    const session = testSession()
    let capturedBody: Record<string, unknown> | undefined
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(async (_url: string, init?: RequestInit) => {
        capturedBody = JSON.parse(init!.body as string)
        return {
          ok: true,
          status: 200,
          text: () => Promise.resolve(JSON.stringify({ id: 'c1', participants: ['a', 'b'] })),
        }
      }),
    )
    await session.createConversation(['a', 'b'])
    expect(capturedBody).toEqual({ participants: ['a', 'b'] })

    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation(async (_url: string, init?: RequestInit) => {
        capturedBody = JSON.parse(init!.body as string)
        return {
          ok: true,
          status: 200,
          text: () =>
            Promise.resolve(JSON.stringify({ id: 'm1', conversation_id: 'c1', author: 'a', body: 'hi', sent_at: 'now' })),
        }
      }),
    )
    const message = await session.sendConversationMessage('c1', 'hi')
    expect(capturedBody).toEqual({ body: 'hi' })
    expect(message).toEqual({ id: 'm1', conversationId: 'c1', author: 'a', body: 'hi', sentAt: 'now' })
  })
})
