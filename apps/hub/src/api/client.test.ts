import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  approveDeviceGrant,
  approvePairing,
  createFriendRequest,
  declineOrWithdrawFriendRequest,
  denyPairing,
  getMe,
  getMyHistory,
  getPresence,
  getProfiles,
  getServerUrl,
  listDeviceGrants,
  listDevices,
  listFriends,
  openPresenceSocket,
  registerStart,
  removeFriend,
  requestDeviceGrant,
  resolveHandle,
  revokeDevice,
  searchIdentities,
  setServerUrl,
  updateProfile,
} from './client'
import { AvalonApiError } from './errors'

// A minimal fake standing in for the browser's `WebSocket` — records what
// was sent, and lets a test drive `open`/`message` events by hand rather
// than depending on a real connection (there is no real server in this
// test environment).
class FakeWebSocket {
  static readonly OPEN = 1
  static instances: FakeWebSocket[] = []

  readyState = 0
  sent: string[] = []
  url: string
  private listeners: Record<string, ((event: unknown) => void)[]> = {}

  constructor(url: string) {
    this.url = url
    FakeWebSocket.instances.push(this)
  }

  addEventListener(type: string, listener: (event: unknown) => void) {
    ;(this.listeners[type] ??= []).push(listener)
  }

  send(data: string) {
    this.sent.push(data)
  }

  close() {}

  emitOpen() {
    this.readyState = FakeWebSocket.OPEN
    for (const listener of this.listeners.open ?? []) listener({})
  }

  emitMessage(data: unknown) {
    for (const listener of this.listeners.message ?? []) {
      listener({ data: JSON.stringify(data) })
    }
  }
}

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
  FakeWebSocket.instances = []
  localStorage.clear()
})

// Issue #232's network selector: setServerUrl/getServerUrl are the only
// reader/writer of the persisted "which server is the Hub pointed at"
// choice — the switcher UI never touches localStorage directly.
describe('getServerUrl/setServerUrl (issue #232)', () => {
  it('falls back to the build-time default when nothing has been chosen', () => {
    expect(getServerUrl()).toBe('http://127.0.0.1:8080')
  })

  it('persists a chosen server URL across calls', () => {
    setServerUrl('https://avalon-test.example')
    expect(getServerUrl()).toBe('https://avalon-test.example')
  })

  it('survives a simulated reload (a fresh read of localStorage)', () => {
    setServerUrl('https://avalon-test.example')
    // getServerUrl always reads localStorage live rather than caching, so
    // this is the same value a freshly-loaded module would see after a
    // real page reload.
    expect(getServerUrl()).toBe('https://avalon-test.example')
  })
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

  it('fetches the caller\'s own event history from GET /me/history', async () => {
    mockFetchOnce(200, [{ event_id: 'evt-1', kind: 'identity.created' }])

    await getMyHistory('the-token')

    const [url, options] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(url).toContain('/me/history')
    expect(options.headers.Authorization).toBe('Bearer the-token')
  })

  it('percent-encodes the handle so "#" survives as a path segment', async () => {
    mockFetchOnce(200, { identity_id: 'id-2' })

    await resolveHandle('the-token', 'alice#4821')

    const [url] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(url).toContain('/friends/handle/alice%234821')
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

  it('sends the ids query param for profile lookups', async () => {
    mockFetchOnce(200, [])
    await getProfiles('token', ['id1', 'id2'])
    const [url] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(url).toContain('/identities/profiles?ids=id1%2Cid2')
  })

  it('short-circuits profile lookups with no ids, never calling fetch', async () => {
    vi.stubGlobal('fetch', vi.fn())
    const result = await getProfiles('token', [])
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

  it('sends the discoverable field on PATCH /me', async () => {
    mockFetchOnce(200, {
      identity_id: 'x',
      identity_created_at: 't',
      display_name: 'x',
      avatar_url: null,
      handle: 'x#0001',
      discoverable: true,
    })
    await updateProfile('token', { discoverable: true })
    const [url, options] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(url).toContain('/me')
    expect(options.method).toBe('PATCH')
    expect(JSON.parse(options.body)).toEqual({ discoverable: true })
  })

  it('surfaces the server\'s own message for an invalid avatar_url', async () => {
    mockFetchOnce(400, {
      error: 'avatar_url must be an http(s) URL of 2048 characters or fewer',
    })
    await expect(
      updateProfile('token', { avatar_url: 'javascript:alert(1)' }),
    ).rejects.toMatchObject({
      status: 400,
      message: 'avatar_url must be an http(s) URL of 2048 characters or fewer',
    } satisfies Partial<AvalonApiError>)
  })
})

describe('searchIdentities (issue #205)', () => {
  it('sends the q query param', async () => {
    mockFetchOnce(200, { results: [] })
    await searchIdentities('token', 'alice')
    const [url] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(url).toContain('/identities/search?q=alice')
  })

  it('short-circuits a blank query, never calling fetch', async () => {
    vi.stubGlobal('fetch', vi.fn())
    const result = await searchIdentities('token', '   ')
    expect(result).toEqual({ results: [] })
    expect(fetch).not.toHaveBeenCalled()
  })
})

describe('device grants (issue #135)', () => {
  it('requestDeviceGrant POSTs to /me/devices/grants', async () => {
    mockFetchOnce(200, { id: 'grant-1', status: 'pending' })

    await requestDeviceGrant('token', {
      requested_signing_public_key: 'abc',
      device_label: 'a device',
    })

    const [url, options] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(url).toContain('/me/devices/grants')
    expect(options.method).toBe('POST')
  })

  it('listDeviceGrants includes the status filter only when given', async () => {
    mockFetchOnce(200, [])

    await listDeviceGrants('token', 'pending')
    let [url] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(url).toContain('/me/devices/grants?status=pending')

    await listDeviceGrants('token')
    ;[url] = (fetch as ReturnType<typeof vi.fn>).mock.calls[1]
    expect(url).toContain('/me/devices/grants')
    expect(url).not.toContain('?status=')
  })

  it('approveDeviceGrant POSTs to the grant-specific approve path', async () => {
    mockFetchOnce(200, { id: 'key-1' })

    await approveDeviceGrant('token', 'grant-1', {
      approver_signing_key_id: 'key-1',
      signature: 'sig',
    })

    const [url, options] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(url).toContain('/me/devices/grants/grant-1/approve')
    expect(options.method).toBe('POST')
  })

  it('listDevices GETs /me/devices', async () => {
    mockFetchOnce(200, [])
    await listDevices('token')
    const [url] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(url).toContain('/me/devices')
  })

  it('revokeDevice POSTs to the device-specific revoke path', async () => {
    mockFetchOnceEmpty(200)
    await revokeDevice('token', 'key-1')
    const [url, options] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(url).toContain('/me/devices/key-1/revoke')
    expect(options.method).toBe('POST')
  })
})

describe('device pairing (issue #307)', () => {
  it('approvePairing POSTs the user_code to /auth/device/approve', async () => {
    mockFetchOnce(200, { status: 'approved' })

    await approvePairing('token', { user_code: 'ABCD2345' })

    const [url, options] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(url).toContain('/auth/device/approve')
    expect(options.method).toBe('POST')
    expect(JSON.parse(options.body)).toEqual({ user_code: 'ABCD2345' })
    expect(options.headers.Authorization).toBe('Bearer token')
  })

  it('denyPairing POSTs the user_code to /auth/device/deny', async () => {
    mockFetchOnce(200, { status: 'denied' })

    await denyPairing('token', { user_code: 'ABCD2345' })

    const [url, options] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(url).toContain('/auth/device/deny')
    expect(options.method).toBe('POST')
  })
})

describe('openPresenceSocket', () => {
  it('connects to ws://…/ws/presence with the token as a query param', () => {
    vi.stubGlobal('WebSocket', FakeWebSocket)

    openPresenceSocket('the-token', () => {})

    expect(FakeWebSocket.instances).toHaveLength(1)
    expect(FakeWebSocket.instances[0].url).toBe(
      'ws://127.0.0.1:8080/ws/presence?token=the-token',
    )
  })

  it('queues a subscribe call made before open, then flushes it once open', () => {
    vi.stubGlobal('WebSocket', FakeWebSocket)

    const socket = openPresenceSocket('the-token', () => {})
    socket.subscribe(['id-1'])
    const fake = FakeWebSocket.instances[0]
    expect(fake.sent).toHaveLength(0)

    fake.emitOpen()

    expect(fake.sent).toEqual([JSON.stringify({ type: 'subscribe', ids: ['id-1'] })])
  })

  it('sends immediately once already open', () => {
    vi.stubGlobal('WebSocket', FakeWebSocket)

    const socket = openPresenceSocket('the-token', () => {})
    FakeWebSocket.instances[0].emitOpen()
    socket.subscribe(['id-2'])

    expect(FakeWebSocket.instances[0].sent).toEqual([
      JSON.stringify({ type: 'subscribe', ids: ['id-2'] }),
    ])
  })

  it('forwards each incoming message to onUpdate', () => {
    vi.stubGlobal('WebSocket', FakeWebSocket)
    const received: unknown[] = []

    openPresenceSocket('the-token', (presence) => received.push(presence))
    FakeWebSocket.instances[0].emitMessage({
      identity_id: 'id-1',
      status: 'Online',
      playing: null,
      updated_at: 'now',
    })

    expect(received).toEqual([
      { identity_id: 'id-1', status: 'Online', playing: null, updated_at: 'now' },
    ])
  })
})
