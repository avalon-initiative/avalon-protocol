import { afterEach, describe, expect, it, vi } from 'vitest'
import { getMe, registerStart } from './client'
import { AvalonApiError } from './errors'

function mockFetchOnce(status: number, body: unknown) {
  vi.stubGlobal(
    'fetch',
    vi.fn().mockResolvedValue({
      ok: status >= 200 && status < 300,
      status,
      json: () => Promise.resolve(body),
    }),
  )
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('api client', () => {
  it('injects the bearer token header only when a token is provided', async () => {
    mockFetchOnce(200, {
      identity_id: 'id',
      identity_created_at: 'now',
      display_name: 'name',
      avatar_url: null,
    })

    await getMe('the-token')

    const [, options] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(options.headers.Authorization).toBe('Bearer the-token')
  })

  it('sends no Authorization header for unauthenticated calls', async () => {
    mockFetchOnce(200, { ticket_id: 't', challenge: { publicKey: {} } })

    await registerStart({ identity_id: 'id', display_name: 'name' })

    const [, options] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(options.headers.Authorization).toBeUndefined()
  })

  it('throws an AvalonApiError with a player-facing message on a non-2xx response', async () => {
    mockFetchOnce(409, { error: 'identity id already taken' })

    await expect(registerStart({ identity_id: 'id', display_name: 'name' })).rejects.toMatchObject(
      { status: 409, message: 'That identity id is already taken.' } satisfies Partial<AvalonApiError>,
    )
  })
})
