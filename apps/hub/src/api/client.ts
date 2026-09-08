// The one module allowed to call `fetch` (issue #55's invariant). Owns the
// base URL and bearer header; every other module in apps/hub goes through
// the typed functions below rather than touching fetch or the URL directly.
import { AvalonApiError, messageForStatus } from './errors'
import type {
  ApproveDeviceGrantRequest,
  ChannelResponse,
  CreateChannelRequest,
  CreateFriendRequestRequest,
  CreateGuildInviteRequest,
  CreateGuildRequest,
  CreateRoleRequest,
  DeviceGrantResponse,
  DeviceResponse,
  FriendRequestResponse,
  FriendshipResponse,
  GuildInviteResponse,
  GuildMemberResponse,
  GuildResponse,
  HistoryEntryResponse,
  MessageResponse,
  MyGuildMembershipResponse,
  PresenceResponse,
  ProfileResponse,
  PublicProfileResponse,
  RegisterFinishRequest,
  RegisterFinishResponse,
  RegisterStartRequest,
  RegisterStartResponse,
  RenameDeviceRequest,
  RequestDeviceGrantRequest,
  ResolveHandleResponse,
  RoleResponse,
  SendMessageRequest,
  SessionFinishRequest,
  SessionFinishResponse,
  SessionStartRequest,
  SessionStartResponse,
  TransferOwnershipRequest,
  UpdateChannelRequest,
  UpdateGuildMemberRequest,
  UpdateGuildRequest,
  UpdatePresenceRequest,
  UpdateProfileRequest,
  UpdateRoleRequest,
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

// GET /identities/profiles?ids=... (issue #161) — batched, public-fields-only
// profile lookup, used to resolve display names for friends/guild rosters.
export function getProfiles(token: string, ids: string[]): Promise<PublicProfileResponse[]> {
  if (ids.length === 0) {
    return Promise.resolve([])
  }
  const params = new URLSearchParams({ ids: ids.join(',') })
  return request(`/identities/profiles?${params.toString()}`, { token })
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

// Guilds, roles, membership (issues #20/#21), matching
// crates/server/src/guilds.rs field-for-field.

export function createGuild(token: string, body: CreateGuildRequest): Promise<GuildResponse> {
  return request('/guilds', { method: 'POST', body, token })
}

export function getGuild(token: string, guildId: string): Promise<GuildResponse> {
  return request(`/guilds/${guildId}`, { token })
}

export function updateGuild(
  token: string,
  guildId: string,
  body: UpdateGuildRequest,
): Promise<GuildResponse> {
  return request(`/guilds/${guildId}`, { method: 'PATCH', body, token })
}

export function listRoles(token: string, guildId: string): Promise<RoleResponse[]> {
  return request(`/guilds/${guildId}/roles`, { token })
}

export function createRole(
  token: string,
  guildId: string,
  body: CreateRoleRequest,
): Promise<RoleResponse> {
  return request(`/guilds/${guildId}/roles`, { method: 'POST', body, token })
}

export function updateRole(
  token: string,
  guildId: string,
  nameIndex: number,
  body: UpdateRoleRequest,
): Promise<RoleResponse> {
  return request(`/guilds/${guildId}/roles/${nameIndex}`, { method: 'PATCH', body, token })
}

export function transferOwnership(
  token: string,
  guildId: string,
  body: TransferOwnershipRequest,
): Promise<GuildResponse> {
  return request(`/guilds/${guildId}/transfer-ownership`, { method: 'POST', body, token })
}

export function associateGame(
  token: string,
  guildId: string,
  gameId: string,
): Promise<GuildResponse> {
  return request(`/guilds/${guildId}/games/${gameId}`, { method: 'POST', token })
}

export function createGuildInvite(
  token: string,
  guildId: string,
  body: CreateGuildInviteRequest,
): Promise<GuildInviteResponse> {
  return request(`/guilds/${guildId}/invites`, { method: 'POST', body, token })
}

export function acceptGuildInvite(
  token: string,
  guildId: string,
  inviteId: string,
): Promise<GuildMemberResponse> {
  return request(`/guilds/${guildId}/invites/${inviteId}/accept`, { method: 'POST', token })
}

export function declineGuildInvite(
  token: string,
  guildId: string,
  inviteId: string,
): Promise<void> {
  return request(`/guilds/${guildId}/invites/${inviteId}/decline`, { method: 'POST', token })
}

export function joinGuild(token: string, guildId: string): Promise<GuildMemberResponse> {
  return request(`/guilds/${guildId}/join`, { method: 'POST', token })
}

export function leaveGuild(token: string, guildId: string): Promise<void> {
  return request(`/guilds/${guildId}/leave`, { method: 'POST', token })
}

export function listMembers(token: string, guildId: string): Promise<GuildMemberResponse[]> {
  return request(`/guilds/${guildId}/members`, { token })
}

export function updateMemberRole(
  token: string,
  guildId: string,
  identityId: string,
  body: UpdateGuildMemberRequest,
): Promise<GuildMemberResponse> {
  return request(`/guilds/${guildId}/members/${identityId}`, { method: 'PATCH', body, token })
}

export function removeMember(token: string, guildId: string, identityId: string): Promise<void> {
  return request(`/guilds/${guildId}/members/${identityId}`, { method: 'DELETE', token })
}

export function listMyGuilds(token: string): Promise<MyGuildMembershipResponse[]> {
  return request('/me/guilds', { token })
}

// Guild channels + messages (issue #22), matching
// crates/server/src/channels.rs and crates/server/src/guild_messages.rs.

export function listChannels(token: string, guildId: string): Promise<ChannelResponse[]> {
  return request(`/guilds/${guildId}/channels`, { token })
}

export function createChannel(
  token: string,
  guildId: string,
  body: CreateChannelRequest,
): Promise<ChannelResponse> {
  return request(`/guilds/${guildId}/channels`, { method: 'POST', body, token })
}

export function updateChannel(
  token: string,
  guildId: string,
  channelId: string,
  body: UpdateChannelRequest,
): Promise<ChannelResponse> {
  return request(`/guilds/${guildId}/channels/${channelId}`, { method: 'PATCH', body, token })
}

export function archiveChannel(
  token: string,
  guildId: string,
  channelId: string,
): Promise<ChannelResponse> {
  return request(`/guilds/${guildId}/channels/${channelId}/archive`, { method: 'POST', token })
}

export function listMessages(
  token: string,
  guildId: string,
  channelId: string,
  options: { before?: string; limit?: number } = {},
): Promise<MessageResponse[]> {
  const params = new URLSearchParams()
  if (options.before) params.set('before', options.before)
  if (options.limit) params.set('limit', String(options.limit))
  const query = params.toString()
  return request(`/guilds/${guildId}/channels/${channelId}/messages${query ? `?${query}` : ''}`, {
    token,
  })
}

export function sendMessage(
  token: string,
  guildId: string,
  channelId: string,
  body: SendMessageRequest,
): Promise<MessageResponse> {
  return request(`/guilds/${guildId}/channels/${channelId}/messages`, {
    method: 'POST',
    body,
    token,
  })
}

// Hard-deletes a message (moderation, not history — see
// crates/server/src/guild_messages.rs's module doc comment). Requires
// `manage_channels`, same as channel management.
export function deleteMessage(
  token: string,
  guildId: string,
  channelId: string,
  messageId: string,
): Promise<{ deleted: boolean }> {
  return request(`/guilds/${guildId}/channels/${channelId}/messages/${messageId}`, {
    method: 'DELETE',
    token,
  })
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
