// The one module allowed to call `fetch` (issue #55's invariant). Owns the
// base URL and bearer header; every other module in apps/hub goes through
// the typed functions below rather than touching fetch or the URL directly.
import { AvalonApiError, messageForStatus } from './errors'
import type {
  CreateFriendRequestRequest,
  FriendRequestResponse,
  FriendshipResponse,
  HistoryEntryResponse,
  PresenceResponse,
  ProfileResponse,
  RegisterFinishRequest,
  RegisterFinishResponse,
  RegisterStartRequest,
  RegisterStartResponse,
  ResolveHandleResponse,
  SessionFinishRequest,
  SessionFinishResponse,
  SessionStartRequest,
  SessionStartResponse,
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
    throw new AvalonApiError(response.status, messageForStatus(response.status))
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

export function getPresence(token: string, ids: string[]): Promise<PresenceResponse[]> {
  if (ids.length === 0) {
    return Promise.resolve([])
  }
  const params = new URLSearchParams({ ids: ids.join(',') })
  return request(`/presence?${params.toString()}`, { token })
}
