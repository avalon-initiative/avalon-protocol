import { afterEach, describe, expect, it } from 'vitest'
import { request } from '../src/http.js'
import { UnauthorizedError } from '../src/errors.js'

const originalFetch = globalThis.fetch

afterEach(() => {
  globalThis.fetch = originalFetch
})

describe('request 401 reconnect (issue #525)', () => {
  it('retries once with the token onUnauthorized mints, and never calls it again on success', async () => {
    let calls = 0
    let onUnauthorizedCalls = 0
    globalThis.fetch = (async (_url: string, init?: RequestInit) => {
      calls += 1
      const auth = (init?.headers as Record<string, string>)?.authorization
      if (auth === 'Bearer stale-token') {
        return new Response('', { status: 401 })
      }
      expect(auth).toBe('Bearer fresh-token')
      return new Response(JSON.stringify({ ok: true }), { status: 200 })
    }) as typeof fetch

    const result = await request<{ ok: boolean }>('http://127.0.0.1:1', '/me', {
      token: 'stale-token',
      onUnauthorized: async (failedToken) => {
        onUnauthorizedCalls += 1
        expect(failedToken).toBe('stale-token')
        return 'fresh-token'
      },
    })

    expect(result).toEqual({ ok: true })
    expect(calls).toBe(2)
    expect(onUnauthorizedCalls).toBe(1)
  })

  it('propagates the 401 when onUnauthorized resolves to null', async () => {
    globalThis.fetch = (async () => new Response('', { status: 401 })) as typeof fetch

    await expect(
      request('http://127.0.0.1:1', '/me', { token: 'stale-token', onUnauthorized: async () => null }),
    ).rejects.toBeInstanceOf(UnauthorizedError)
  })

  it('propagates the 401 as-is when no onUnauthorized hook is given', async () => {
    let calls = 0
    globalThis.fetch = (async () => {
      calls += 1
      return new Response('', { status: 401 })
    }) as typeof fetch

    await expect(request('http://127.0.0.1:1', '/me', { token: 'stale-token' })).rejects.toBeInstanceOf(
      UnauthorizedError,
    )
    expect(calls).toBe(1)
  })

  it('never fires onUnauthorized for a non-401 error', async () => {
    let onUnauthorizedCalls = 0
    globalThis.fetch = (async () => new Response('', { status: 404 })) as typeof fetch

    await expect(
      request('http://127.0.0.1:1', '/me', {
        token: 'stale-token',
        onUnauthorized: async () => {
          onUnauthorizedCalls += 1
          return 'fresh-token'
        },
      }),
    ).rejects.toThrow()
    expect(onUnauthorizedCalls).toBe(0)
  })
})
