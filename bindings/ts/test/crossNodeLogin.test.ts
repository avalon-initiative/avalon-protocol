import { afterEach, describe, expect, it } from 'vitest'
import { startCrossNodeLogin, pollCrossNodeLogin } from '../src/crossNodeLogin.js'

const originalFetch = globalThis.fetch

afterEach(() => {
  globalThis.fetch = originalFetch
})

function mockFetchOnce(body: unknown, status = 200) {
  globalThis.fetch = (async () => new Response(JSON.stringify(body), { status })) as typeof fetch
}

describe('startCrossNodeLogin', () => {
  it('converts wire snake_case into camelCase', async () => {
    mockFetchOnce({
      request_code: 'rc1',
      user_code: 'ABCD-1234',
      requesting_context: 'http://requester',
      expires_in: 300,
      poll_interval: 2,
    })
    const result = await startCrossNodeLogin('http://requester')
    expect(result).toEqual({
      requestCode: 'rc1',
      userCode: 'ABCD-1234',
      requestingContext: 'http://requester',
      expiresIn: 300,
      pollInterval: 2,
    })
  })
})

describe('pollCrossNodeLogin', () => {
  it('sends requestCode as the bearer token and converts a pending response', async () => {
    let capturedAuth: string | null = null
    globalThis.fetch = (async (_url, init) => {
      capturedAuth = (init?.headers as Record<string, string>)?.authorization ?? null
      return new Response(JSON.stringify({ status: 'pending' }), { status: 200 })
    }) as typeof fetch

    const result = await pollCrossNodeLogin('http://requester', 'rc1')
    expect(result).toEqual({ status: 'pending', token: undefined, expiresAt: undefined })
    expect(capturedAuth).toBe('Bearer rc1')
  })

  it('converts an approved response with a token', async () => {
    mockFetchOnce({ status: 'approved', token: 'session-token', expires_at: '2026-01-01T00:00:00Z' })
    const result = await pollCrossNodeLogin('http://requester', 'rc1')
    expect(result).toEqual({ status: 'approved', token: 'session-token', expiresAt: '2026-01-01T00:00:00Z' })
  })
})
