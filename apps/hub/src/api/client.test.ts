import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  createFriendRequest,
  declineOrWithdrawFriendRequest,
  getMe,
  getPresence,
  listFriends,
  registerStart,
  removeFriend,
} from './client'
import { AvalonApiError } from './errors'

function mockFetchOnce(status: number, body: unknown) {
  vi.stubGlobal(
    'fetch',
    vi.fn().mockResolvedValue({
      ok: status >= 200 && status < 300,
      status,
      json: () => Promise.resolve(body),
      text: () => Promise.resolve(JSON.stringify(body)),
    }),
  )
}

// For endpoints that return an empty 200 body (e.g. the friends
// DELETE/decline routes — see the comment on `request()` in ./client).
function mockFetchOnceEmpty(status: number) {
  vi.stubGlobal(
    'fetch',
    vi.fn().mockResolvedValue({
      ok: status >= 200 && status < 300,
      status,
      json: () => Promise.reject(new Error('no body to parse as json')),
      text: () => Promise.resolve(''),
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

  it('parses a normal JSON friends list response', async () => {
    mockFetchOnce(200, [{ a: 'x', b: 'y', since: 't' }])
    const friends = await listFriends('token')
    expect(friends).toEqual([{ a: 'x', b: 'y', since: 't' }])
  })

  it('sends the ids query param for presence lookups', async () => {
    mockFetchOnce(200, [])
    await getPresence('token', ['id1', 'id2'])
    const [url] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(url).toContain('/presence?ids=id1%2Cid2')
  })

  it('short-circuits presence lookups with no ids, never calling fetch', async () => {
    vi.stubGlobal('fetch', vi.fn())
    const result = await getPresence('token', [])
    expect(result).toEqual([])
    expect(fetch).not.toHaveBeenCalled()
  })

  it('sends the friend request body correctly', async () => {
    mockFetchOnce(200, { id: 'r1', from: 'x', to: 'y', requested_at: 't' })
    await createFriendRequest('token', { to: 'y' })
    const [, options] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(JSON.parse(options.body)).toEqual({ to: 'y' })
  })

  // decline_or_withdraw_friend_request and remove_friend are Rust handlers
  // returning Result<(), AppError>, which axum serializes as 200 with an
  // empty body, not 204 — request() has to handle that generically or
  // .json() throws parsing zero bytes. See the comment on request() itself.
  it('handles an empty 200 body from declining/withdrawing a request', async () => {
    mockFetchOnceEmpty(200)
    await expect(declineOrWithdrawFriendRequest('token', 'r1')).resolves.toBeUndefined()
  })

  it('handles an empty 200 body from removing a friend', async () => {
    mockFetchOnceEmpty(200)
    await expect(removeFriend('token', 'other-id')).resolves.toBeUndefined()
  })

  it('maps a 404 on an already-resolved request to a clear message', async () => {
    mockFetchOnce(404, { error: 'friend request not found' })
    await expect(declineOrWithdrawFriendRequest('token', 'r1')).rejects.toMatchObject({
      status: 404,
      message: "That's no longer there — it may have already been handled.",
    } satisfies Partial<AvalonApiError>)
  })
})
