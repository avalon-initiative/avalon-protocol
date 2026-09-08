// The one module allowed to call `fetch` (issue #55's invariant). Owns the
// base URL and bearer header; every other module in apps/hub goes through
// the typed functions below rather than touching fetch or the URL directly.
import { AvalonApiError, messageForStatus } from './errors'
import type {
  ApproveDeviceGrantRequest,
  CreateFriendRequestRequest,
  DeviceGrantResponse,
  DeviceResponse,
  FriendRequestResponse,
  FriendshipResponse,
  HistoryEntryResponse,
  PresenceResponse,
  ProfileResponse,
  RegisterFinishRequest,
  RegisterFinishResponse,
  RegisterStartRequest,
  RegisterStartResponse,
  RenameDeviceRequest,
  RequestDeviceGrantRequest,
  ResolveHandleResponse,
  SessionFinishRequest,
  SessionFinishResponse,
  SessionStartRequest,
  SessionStartResponse,
  UpdatePresenceRequest,
  UpdateProfileRequest,
} from './types'

const BASE_URL = import.meta.env.VITE_AVALON_SERVER_URL ?? 'http://127.0.0.1:8080'

async function request<T>(
  path: string,
  options: { method?: string; body?: unknown; token?: string } = {},
): Promise<T> {
  const headers: Record<string, string> = { 'Content-Type': 'application/json' }
  if (options.token) {
    headers.Authorization = `Bearer ${options.token}`
  }

  const response = await fetch(`${BASE_URL}${path}`, {
    method: options.method ?? 'GET',
    headers,
    body: options.body !== undefined ? JSON.stringify(options.body) : undefined,
  })

  if (!response.ok) {
    // avalon-server's `{ "error": "..." }` body is already written to be
    // safe to show a player (crates/server/src/error.rs's own convention —
    // every variant except a raw database/ledger failure gets real,
    // specific text) — read it when present so a 400 that isn't the
    // ceremony-expiry case messageForStatus was originally written for
    // (e.g. an invalid avatar_url) doesn't show a misleading fallback.
    let serverMessage: string | undefined
    try {
      const body: unknown = await response.json()
      if (body && typeof body === 'object' && typeof (body as { error?: unknown }).error === 'string') {
        serverMessage = (body as { error: string }).error
      }
    } catch {
      // Body wasn't JSON (or was empty) — fall back to the generic mapping.
    }
    throw new AvalonApiError(response.status, messageForStatus(response.status, serverMessage))
  }

  // Some endpoints (e.g. the friend-request DELETE/decline routes) are
  // Rust handlers returning `Result<(), AppError>`, which axum serializes
  // as 200 with an empty body, not 204 — so an empty body has to be
  // handled generically rather than gated on a specific status code, or
  // `.json()` throws trying to parse zero bytes.
  const text = await response.text()
  return (text.length === 0 ? undefined : JSON.parse(text)) as T
}

export function registerStart(body: RegisterStartRequest): Promise<RegisterStartResponse> {
  return request('/identities/register/start', { method: 'POST', body })
}

export function registerFinish(body: RegisterFinishRequest): Promise<RegisterFinishResponse> {
  return request('/identities/register/finish', { method: 'POST', body })
}

export function sessionStart(body: SessionStartRequest): Promise<SessionStartResponse> {
  return request('/sessions/start', { method: 'POST', body })
}

export function sessionFinish(body: SessionFinishRequest): Promise<SessionFinishResponse> {
  return request('/sessions/finish', { method: 'POST', body })
}

export function getMe(token: string): Promise<ProfileResponse> {
  return request('/me', { token })
}

export function updateProfile(token: string, body: UpdateProfileRequest): Promise<ProfileResponse> {
  return request('/me', { method: 'PATCH', body, token })
}

export function getMyHistory(token: string): Promise<HistoryEntryResponse[]> {
  return request('/me/history', { token })
}

export function listFriends(token: string): Promise<FriendshipResponse[]> {
  return request('/friends', { token })
}

export function listFriendRequests(token: string): Promise<FriendRequestResponse[]> {
  return request('/friends/requests', { token })
}

export function createFriendRequest(
  token: string,
  body: CreateFriendRequestRequest,
): Promise<FriendRequestResponse> {
  return request('/friends/requests', { method: 'POST', body, token })
}

export function acceptFriendRequest(token: string, requestId: string): Promise<FriendshipResponse> {
  return request(`/friends/requests/${requestId}/accept`, { method: 'POST', token })
}

// Declines an incoming request or withdraws an outgoing one — the server
// infers which from who's calling (crates/server/src/friends.rs).
export function declineOrWithdrawFriendRequest(token: string, requestId: string): Promise<void> {
  return request(`/friends/requests/${requestId}`, { method: 'DELETE', token })
}

export function removeFriend(token: string, identityId: string): Promise<void> {
  return request(`/friends/${identityId}`, { method: 'DELETE', token })
}

// Resolves a `display_name#1234` handle (issue #128) to an identity id for
// the "add friend" flow — exact match only. `#` isn't safe unencoded in a
// URL path segment, so it's escaped here rather than left to the caller.
export function resolveHandle(token: string, handle: string): Promise<ResolveHandleResponse> {
  return request(`/friends/handle/${encodeURIComponent(handle)}`, { token })
}

// PUT /me/presence — the caller publishing their own status. The shell
// heartbeats this while the Hub is open so the player actually reads as
// Online to their friends (the store's TTL expires a stale entry to
// Offline otherwise — see crates/server/src/presence.rs).
export function updateMyPresence(
  token: string,
  body: UpdatePresenceRequest,
): Promise<PresenceResponse> {
  return request('/me/presence', { method: 'PUT', body, token })
}

export function getPresence(token: string, ids: string[]): Promise<PresenceResponse[]> {
  if (ids.length === 0) {
    return Promise.resolve([])
  }
  const params = new URLSearchParams({ ids: ids.join(',') })
  return request(`/presence?${params.toString()}`, { token })
}

// Device-registration / linked-device grant model (issue #135).

export function requestDeviceGrant(
  token: string,
  body: RequestDeviceGrantRequest,
): Promise<DeviceGrantResponse> {
  return request('/me/devices/grants', { method: 'POST', body, token })
}

export function listDeviceGrants(
  token: string,
  status?: DeviceGrantResponse['status'],
): Promise<DeviceGrantResponse[]> {
  const path = status ? `/me/devices/grants?status=${status}` : '/me/devices/grants'
  return request(path, { token })
}

export function getDeviceGrant(token: string, grantId: string): Promise<DeviceGrantResponse> {
  return request(`/me/devices/grants/${grantId}`, { token })
}

export function approveDeviceGrant(
  token: string,
  grantId: string,
  body: ApproveDeviceGrantRequest,
): Promise<DeviceResponse> {
  return request(`/me/devices/grants/${grantId}/approve`, { method: 'POST', body, token })
}

export function listDevices(token: string): Promise<DeviceResponse[]> {
  return request('/me/devices', { token })
}

export function renameDevice(
  token: string,
  signingKeyId: string,
  body: RenameDeviceRequest,
): Promise<DeviceResponse> {
  return request(`/me/devices/${signingKeyId}`, { method: 'PATCH', body, token })
}

export function revokeDevice(token: string, signingKeyId: string): Promise<void> {
  return request(`/me/devices/${signingKeyId}/revoke`, { method: 'POST', token })
}

// BASE_URL is http(s)://…; the websocket endpoint needs ws(s)://… — same
// scheme swap crates/sdk/src/social.rs's websocket_url does, ported here
// since the Hub doesn't consume the Rust SDK directly.
function websocketUrl(path: string): string {
  if (BASE_URL.startsWith('https://')) {
    return `wss://${BASE_URL.slice('https://'.length)}${path}`
  }
  if (BASE_URL.startsWith('http://')) {
    return `ws://${BASE_URL.slice('http://'.length)}${path}`
  }
  return `${BASE_URL}${path}`
}

export interface PresenceSocket {
  // Additive — calling this again with more ids grows the subscription
  // rather than replacing it, mirroring the server's own `ClientMessage`
  // semantics (crates/server/src/presence.rs). Queued until the socket
  // finishes connecting if called before `open`.
  subscribe(ids: string[]): void
  close(): void
}

// GET /ws/presence (issue #136) — live presence push, additive to
// getPresence's point-in-time reads. `onUpdate` fires once per pushed
// PresenceResponse, including the immediate catch-up snapshot the server
// sends for each newly-subscribed id (so a caller doesn't need a separate
// getPresence call just to get the current state before the first push).
export function openPresenceSocket(
  token: string,
  onUpdate: (presence: PresenceResponse) => void,
): PresenceSocket {
  const socket = new WebSocket(websocketUrl(`/ws/presence?token=${encodeURIComponent(token)}`))
  const pendingSubscriptions: string[][] = []

  function send(ids: string[]) {
    socket.send(JSON.stringify({ type: 'subscribe', ids }))
  }

  socket.addEventListener('open', () => {
    for (const ids of pendingSubscriptions.splice(0)) {
      send(ids)
    }
  })
  socket.addEventListener('message', (event) => {
    onUpdate(JSON.parse(event.data as string) as PresenceResponse)
  })

  return {
    subscribe(ids: string[]) {
      if (ids.length === 0) return
      if (socket.readyState === WebSocket.OPEN) {
        send(ids)
      } else {
        pendingSubscriptions.push(ids)
      }
    },
    close() {
      socket.close()
    },
  }
}
