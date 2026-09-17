// The one module allowed to call `fetch` (issue #55's invariant). Owns the
// base URL and bearer header; every other module in apps/hub goes through
// the typed functions below rather than touching fetch or the URL directly.
import { AvalonApiError, messageForStatus } from './errors'
import { mintContinuationToken } from '../crypto/continuation'
import { loadSigningKey } from '../crypto/signingKey'
import type {
  AchievementDefinitionResponse,
  AddPasskeyFinishRequest,
  AddPasskeyStartResponse,
  ApproveDeviceGrantRequest,
  ArchivedMessageResponse,
  BlockListEntry,
  BlockResponse,
  CancelRecoveryRequest,
  CreateBlockRequest,
  ChannelResponse,
  ConnectIntegratorRequest,
  ConnectIntegratorResponse,
  ConversationMessageResponse,
  ConversationResponse,
  CreateChannelRequest,
  CreateConversationRequest,
  CreateEventRequest,
  CreateFriendRequestRequest,
  CreateGuildInviteRequest,
  CreateGuildRequest,
  CreateJoinRequestRequest,
  CreateRoleRequest,
  DeviceGrantResponse,
  DeviceResponse,
  DiscoverGuildsResponse,
  DiscoverPeopleResponse,
  EventResponse,
  FavoriteGamesResponse,
  FriendRequestResponse,
  FriendshipResponse,
  GameBreakdownResponse,
  IntegratorRegistryResponse,
  IssuerKeyResponse,
  IntegratorResponse,
  GuardianOfSummary,
  GuardianRequestSummary,
  GuardianSettingsResponse,
  GuildAnnouncementAlert,
  GuildInviteResponse,
  GuildJoinRequestResponse,
  GuildMemberResponse,
  GuildResourceKind,
  GuildResponse,
  HistoryEntryResponse,
  ListEventsQuery,
  ListIntegratorsResponse,
  ListMyAchievementsResponse,
  MessageResponse,
  MyConnectionsResponse,
  MyGuildInviteResponse,
  MyGuildMembershipResponse,
  PasskeyResponse,
  PermissionOverrideResponse,
  PresenceResponse,
  ProfileResponse,
  PublicIdentityProfileResponse,
  PublicProfileResponse,
  RecoveryFinishRequest,
  RecoveryRequestResponse,
  RecoveryStartRequest,
  RecoveryStartResponse,
  RegisterFinishRequest,
  RegisterFinishResponse,
  RegisterStartRequest,
  RegisterStartResponse,
  RenameDeviceRequest,
  RenamePasskeyRequest,
  RequestDeviceGrantRequest,
  ResolveHandleResponse,
  ResolvePairingResponse,
  RoleResponse,
  RsvpRequest,
  RsvpResponse,
  RsvpRosterEntry,
  SearchIdentitiesResponse,
  SendConversationMessageRequest,
  SendMessageRequest,
  SessionFinishRequest,
  SessionFinishResponse,
  SessionStartRequest,
  SessionStartResponse,
  SetGuardiansRequest,
  SetPermissionOverrideRequest,
  SignedTreeHeadResponse,
  TransferOwnershipRequest,
  UpdateChannelRequest,
  UpdateEventRequest,
  UpdateGuildMemberRequest,
  UpdateGuildRequest,
  UpdatePresenceRequest,
  UpdateProfileRequest,
  UpdateRoleRequest,
  UserCodeRequest,
  VisibleIntegratorDataInstanceResponse,
} from './types'

// Issue #232's network selector needs to switch which server the Hub talks
// to at runtime, not just at build time — `localStorage` (checked first)
// lets a viewer's choice persist across reloads; `VITE_AVALON_SERVER_URL`
// stays the build-time default for a Hub that's never had one explicitly
// picked. `setServerUrl`/`getServerUrl` are the only writer/reader of this
// storage key so there's exactly one place that owns "what network am I
// currently pointed at" — the selector UI never touches `localStorage`
// directly.
const SERVER_URL_STORAGE_KEY = 'avalon.serverUrl'

function readStoredServerUrl(): string | null {
  try {
    return localStorage.getItem(SERVER_URL_STORAGE_KEY)
  } catch {
    // Private browsing / storage disabled — fall back to the build-time
    // default rather than throwing on every request.
    return null
  }
}

function currentBaseUrl(): string {
  return (
    readStoredServerUrl() ?? import.meta.env.VITE_AVALON_SERVER_URL ?? 'http://127.0.0.1:8080'
  )
}

/** The server URL the Hub is currently configured to talk to. */
export function getServerUrl(): string {
  return currentBaseUrl()
}

/**
 * Switches which server the Hub talks to, persisted across reloads. Does
 * NOT itself reload the page or reset any in-memory session state — a
 * caller (the network selector) is expected to reload immediately after,
 * since an existing session's bearer token/state was established against
 * the *previous* server and has no meaning against a different one.
 */
export function setServerUrl(url: string) {
  try {
    localStorage.setItem(SERVER_URL_STORAGE_KEY, url)
  } catch {
    // Same private-browsing/storage-disabled case as readStoredServerUrl —
    // nothing to persist to, but don't throw and break the switch flow.
  }
}

const BASE_URL = currentBaseUrl()

// Issue #525's reconnect trigger reads these directly rather than
// importing the Pinia session store — same reasoning `SERVER_URL_STORAGE_KEY`
// above already established: this module owns fetch/localStorage access
// and must not depend on a store that itself depends on this module.
// Values must match `stores/session.ts`'s own storage keys exactly.
const SESSION_TOKEN_STORAGE_KEY = 'avalon:session:token'
const SESSION_IDENTITY_ID_STORAGE_KEY = 'avalon:session:identityId'
const SESSION_SIGNING_KEY_ID_STORAGE_KEY = 'avalon:session:signingKeyId'

/**
 * Issue #525: the actual "detect my node is unreachable, mint a
 * continuation token, reconnect elsewhere" trigger. Only fires when
 * `failedToken` is exactly the currently-persisted opaque session token
 * (never for some other bearer value a caller passed directly, e.g. an
 * identity token mid-registration) — that's the specific case a 401 here
 * can mean "this session's origin node doesn't recognize this token,"
 * whether because the Hub just switched `server_url` (`NetworkStatus.vue`)
 * or because the original node went offline and the viewer is now, for
 * whatever operational reason, reaching a different one at the same URL.
 * Returns `null` when reconnecting isn't possible (no cached identity/
 * signing-key-id, or this device holds no local signing key for it) —
 * the caller falls back to surfacing the 401 as-is, same as before this
 * existed.
 */
function tryMintReconnectToken(failedToken: string): string | null {
  let storedToken: string | null
  let identityId: string | null
  let signingKeyId: string | null
  try {
    storedToken = localStorage.getItem(SESSION_TOKEN_STORAGE_KEY)
    identityId = localStorage.getItem(SESSION_IDENTITY_ID_STORAGE_KEY)
    signingKeyId = localStorage.getItem(SESSION_SIGNING_KEY_ID_STORAGE_KEY)
  } catch {
    return null
  }
  if (!storedToken || storedToken !== failedToken || !identityId || !signingKeyId) {
    return null
  }
  const secretKey = loadSigningKey(identityId)
  if (!secretKey) return null
  return mintContinuationToken(identityId, signingKeyId, secretKey)
}

async function request<T>(
  path: string,
  options: { method?: string; body?: unknown; token?: string } = {},
): Promise<T> {
  const headers: Record<string, string> = { 'Content-Type': 'application/json' }
  if (options.token) {
    headers.Authorization = `Bearer ${options.token}`
  }

  let response = await fetch(`${BASE_URL}${path}`, {
    method: options.method ?? 'GET',
    headers,
    body: options.body !== undefined ? JSON.stringify(options.body) : undefined,
  })

  // Issue #525: exactly one retry, with a freshly-minted continuation
  // token in place of the stale opaque one — never persisted back into
  // `avalon:session:token` (a continuation token is single-use/short-lived
  // by design, see crypto/continuation.ts), so every later request against
  // a node that still doesn't recognize the opaque token mints its own
  // fresh one again here, same as this one just did.
  if (response.status === 401 && options.token) {
    const reconnectToken = tryMintReconnectToken(options.token)
    if (reconnectToken) {
      response = await fetch(`${BASE_URL}${path}`, {
        method: options.method ?? 'GET',
        headers: { ...headers, Authorization: `Bearer ${reconnectToken}` },
        body: options.body !== undefined ? JSON.stringify(options.body) : undefined,
      })
    }
  }

  if (!response.ok) {
    // avalon-server's `{ "error": "..." }` body is already written to be
    // safe to show a user (crates/server/src/error.rs's own convention —
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

// Cross-device pairing (issue #307): bootstraps a session for a
// WebAuthn-incapable client. `approvePairing`/`denyPairing` use the
// user's existing authenticated Hub session — no new auth surface.

export function approvePairing(
  token: string,
  body: UserCodeRequest,
): Promise<ResolvePairingResponse> {
  return request('/auth/device/approve', { method: 'POST', body, token })
}

export function denyPairing(
  token: string,
  body: UserCodeRequest,
): Promise<ResolvePairingResponse> {
  return request('/auth/device/deny', { method: 'POST', body, token })
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

// Issue #280: recent announcement-only channel posts across every guild
// the caller currently belongs to. Read-only — no unread/seen tracking
// server-side, see api/guildAnnouncements.ts.
export function getMyGuildAnnouncements(token: string): Promise<GuildAnnouncementAlert[]> {
  return request('/me/guild-announcements', { token })
}

// Issue #34/#35/#377: the caller's own attestation history across every
// issuer (active and revoked alike) — see AttestationResponse's own doc
// comment for why there's deliberately no recognition field here.
// `limit=200` (the server's max page size) rather than the endpoint's
// default 50, matching the Rust SDK's own #377 scope decision: minimize
// the behavior change from before pagination landed, without building
// "load more" UI in this same pass.
export function getMyAchievements(token: string): Promise<ListMyAchievementsResponse> {
  return request('/me/achievements?limit=200', { token })
}

// Public, unauthenticated (crates/server/src/achievements.rs) — used by
// apps/hub/src/api/achievements.ts to resolve a claim's display name,
// which AttestationResponse itself doesn't carry (only the definition's
// GlobalId string).
export function listAchievementDefinitions(slug: string): Promise<AchievementDefinitionResponse[]> {
  return request(`/integrations/${slug}/achievements`)
}

export function listMilestoneDefinitions(slug: string): Promise<AchievementDefinitionResponse[]> {
  return request(`/integrations/${slug}/milestones`)
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

// Issue #97. `listBlocks` returns only the caller's own outgoing blocks —
// see crates/server/src/blocks.rs's module doc comment on why the blocked
// party is never told, through any endpoint.
export function listBlocks(token: string): Promise<BlockListEntry[]> {
  return request('/blocks', { token })
}

export function createBlock(token: string, body: CreateBlockRequest): Promise<BlockResponse> {
  return request('/blocks', { method: 'POST', body, token })
}

export function removeBlock(token: string, identityId: string): Promise<void> {
  return request(`/blocks/${identityId}`, { method: 'DELETE', token })
}

// Resolves a display_name handle (issue #128, #510 — display_name itself
// is the globally-unique handle now) to an identity id for the "add
// friend" flow — exact match only. URL-encoded since a display name can
// contain characters that aren't safe unencoded in a URL path segment.
export function resolveHandle(token: string, handle: string): Promise<ResolveHandleResponse> {
  return request(`/friends/handle/${encodeURIComponent(handle)}`, { token })
}

// GET /people/discover (issue #204) — "people you may know", scoped to
// friends-of-friends and mutual guild membership. No parameters: the
// caller's own session is the only input, never a search term.
export function discoverPeople(token: string): Promise<DiscoverPeopleResponse> {
  return request('/people/discover', { token })
}

// GET /identities/search?q=&limit= (issue #205) — the opt-in global
// name/handle search counterpart to discoverPeople above. Matches only
// identities that have turned on their own `discoverable` preference (see
// updateProfile's `discoverable` field). An empty/blank query returns no
// results without a round trip.
export function searchIdentities(token: string, q: string): Promise<SearchIdentitiesResponse> {
  if (q.trim().length === 0) {
    return Promise.resolve({ results: [] })
  }
  const params = new URLSearchParams({ q })
  return request(`/identities/search?${params.toString()}`, { token })
}

// PUT /me/presence — the caller publishing their own status. The shell
// heartbeats this while the Hub is open so the user actually reads as
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

// GET /identities/:id/profile (issue #403) — a single identity's full
// self-description profile, same exposure level as GET /me, for a real
// profile-card view rather than a roster resolve.
export function getIdentityProfile(
  token: string,
  identityId: string,
): Promise<PublicIdentityProfileResponse> {
  return request(`/identities/${identityId}/profile`, { token })
}

// GET /identities/:id/integrator-data (#384) — public, unauthenticated
// (no token param, matching getIntegratorPublic's own posture below):
// every current instance a connected integrator has published about this
// identity, already filtered server-side to only the fields that
// integrator's schema currently makes visible.
export function getIdentityIntegratorData(
  identityId: string,
): Promise<VisibleIntegratorDataInstanceResponse[]> {
  return request(`/identities/${identityId}/integrator-data`)
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

// Multi-passkey registration (issue #200): WebAuthn login credentials,
// distinct from the signing-key device list above.

export function startAddPasskey(token: string): Promise<AddPasskeyStartResponse> {
  return request('/me/passkeys/register/start', { method: 'POST', token })
}

export function finishAddPasskey(
  token: string,
  body: AddPasskeyFinishRequest,
): Promise<PasskeyResponse> {
  return request('/me/passkeys/register/finish', { method: 'POST', body, token })
}

export function listPasskeys(token: string): Promise<PasskeyResponse[]> {
  return request('/me/passkeys', { token })
}

export function renamePasskey(
  token: string,
  passkeyId: string,
  body: RenamePasskeyRequest,
): Promise<PasskeyResponse> {
  return request(`/me/passkeys/${passkeyId}`, { method: 'PATCH', body, token })
}

// `confirm` must be passed as `true` to revoke the identity's last
// remaining passkey (crates/server/src/passkeys.rs's explicit-confirmation
// invariant) — omitted otherwise, matching every other optional-query-param
// convention in this file (see getPresence's `ids`).
export function revokePasskey(
  token: string,
  passkeyId: string,
  confirm?: boolean,
): Promise<void> {
  const path = confirm
    ? `/me/passkeys/${passkeyId}/revoke?confirm=true`
    : `/me/passkeys/${passkeyId}/revoke`
  return request(path, { method: 'POST', token })
}

// Social recovery (issue #201), matching crates/server/src/recovery.rs.
// `startRecoveryRequest`/`finishRecoveryRequest` are the one pair of
// exported functions here that must never be called with a `token` — the
// whole premise is the caller has none for the identity being recovered.

export function getGuardians(token: string): Promise<GuardianSettingsResponse> {
  return request('/me/recovery/guardians', { token })
}

export function setGuardians(
  token: string,
  body: SetGuardiansRequest,
): Promise<GuardianSettingsResponse> {
  return request('/me/recovery/guardians', { method: 'PUT', body, token })
}

export function getMyRecoveryStatus(token: string): Promise<RecoveryRequestResponse | null> {
  return request('/me/recovery/status', { token })
}

export function getGuardianRequests(token: string): Promise<GuardianRequestSummary[]> {
  return request('/me/recovery/guardian-requests', { token })
}

// Issue #443: identities relying on the caller as a guardian, and the
// caller's own opt-out self-removal from one of those designations.
export function getGuardianOf(token: string): Promise<GuardianOfSummary[]> {
  return request('/me/recovery/guardian-of', { token })
}

export function resignAsGuardian(token: string, identityId: string): Promise<void> {
  return request(`/me/recovery/guardian-of/${identityId}`, { method: 'DELETE', token })
}

export function startRecoveryRequest(body: RecoveryStartRequest): Promise<RecoveryStartResponse> {
  return request('/recovery/requests/start', { method: 'POST', body })
}

export function finishRecoveryRequest(
  body: RecoveryFinishRequest,
): Promise<RecoveryRequestResponse> {
  return request('/recovery/requests/finish', { method: 'POST', body })
}

export function getRecoveryRequest(requestId: string): Promise<RecoveryRequestResponse> {
  return request(`/recovery/requests/${requestId}`)
}

export function approveRecoveryRequest(
  token: string,
  requestId: string,
): Promise<RecoveryRequestResponse> {
  return request(`/recovery/requests/${requestId}/approve`, { method: 'POST', token })
}

export function cancelRecoveryRequest(
  token: string,
  requestId: string,
  body: CancelRecoveryRequest,
): Promise<RecoveryRequestResponse> {
  return request(`/recovery/requests/${requestId}/cancel`, { method: 'POST', body, token })
}

export function finalizeRecoveryRequest(requestId: string): Promise<RecoveryRequestResponse> {
  return request(`/recovery/requests/${requestId}/finalize`, { method: 'POST' })
}

export function getIdentityRecoveryStatus(
  identityId: string,
): Promise<RecoveryRequestResponse | null> {
  return request(`/identities/${identityId}/recovery/status`)
}

// Guilds, roles, membership (issues #20/#21), matching
// crates/server/src/guilds.rs field-for-field.

export function createGuild(token: string, body: CreateGuildRequest): Promise<GuildResponse> {
  return request('/guilds', { method: 'POST', body, token })
}

export function getGuild(token: string, guildId: string): Promise<GuildResponse> {
  return request(`/guilds/${guildId}`, { token })
}

// GET /guilds/discover (issue #154) — a browsable/searchable listing of
// guilds, same public-metadata visibility as getGuild above. The query
// string is built by api/guilds.ts::buildDiscoverQueryString so that logic
// stays testable without a fetch.
export function discoverGuilds(token: string, queryString: string): Promise<DiscoverGuildsResponse> {
  return request(`/guilds/discover${queryString}`, { token })
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

export function deleteRole(token: string, guildId: string, nameIndex: number): Promise<void> {
  return request(`/guilds/${guildId}/roles/${nameIndex}`, { method: 'DELETE', token })
}

// Per-resource permission overrides (issue #250), matching
// crates/server/src/guilds.rs's `/guilds/{id}/permission-overrides` routes.

export function listPermissionOverrides(
  token: string,
  guildId: string,
  resourceKind: GuildResourceKind,
  resourceId: string,
): Promise<PermissionOverrideResponse[]> {
  const query = new URLSearchParams({ resource_kind: resourceKind, resource_id: resourceId })
  return request(`/guilds/${guildId}/permission-overrides?${query.toString()}`, { token })
}

export function setPermissionOverride(
  token: string,
  guildId: string,
  body: SetPermissionOverrideRequest,
): Promise<PermissionOverrideResponse> {
  return request(`/guilds/${guildId}/permission-overrides`, { method: 'PUT', body, token })
}

export function deletePermissionOverride(
  token: string,
  guildId: string,
  overrideId: string,
): Promise<void> {
  return request(`/guilds/${guildId}/permission-overrides/${overrideId}`, {
    method: 'DELETE',
    token,
  })
}

export function transferOwnership(
  token: string,
  guildId: string,
  body: TransferOwnershipRequest,
): Promise<GuildResponse> {
  return request(`/guilds/${guildId}/transfer-ownership`, { method: 'POST', body, token })
}

export function associateIntegrator(
  token: string,
  guildId: string,
  integratorId: string,
): Promise<GuildResponse> {
  return request(`/guilds/${guildId}/integrations/${integratorId}`, { method: 'POST', token })
}

// GET /guilds/{id}/integrator-breakdown (issue #206) — gated server-side to a
// manage_guild holder (always) or anyone when the guild has set
// `game_breakdown_public` (see api/guilds.ts's own note and
// crates/server/src/guilds.rs::game_breakdown). A 403 here is expected and
// handled by the caller, not a bug.
export function getGameBreakdown(token: string, guildId: string): Promise<GameBreakdownResponse> {
  return request(`/guilds/${guildId}/integrator-breakdown`, { token })
}

// GET/PUT /guilds/{id}/favorite-integrators (issue #207, implementing decision
// #160): a manage_guild-curated top-5 pin list drawn only from integrators with
// real affinity per getGameBreakdown above. GET is unrestricted (same
// visibility as getGuild — favorites are always part of the public
// profile, see GuildResponse.favorite_games); PUT is manage_guild-gated
// server-side and always sends the full desired ordered list.
export function getFavoriteGames(token: string, guildId: string): Promise<FavoriteGamesResponse> {
  return request(`/guilds/${guildId}/favorite-integrators`, { token })
}

export function setFavoriteGames(
  token: string,
  guildId: string,
  integratorIds: string[],
): Promise<FavoriteGamesResponse> {
  return request(`/guilds/${guildId}/favorite-integrators`, {
    method: 'PUT',
    body: { integrator_ids: integratorIds },
    token,
  })
}

// Issue #442: the invitee's own pending invites, across every guild.
export function getMyGuildInvites(token: string): Promise<MyGuildInviteResponse[]> {
  return request('/me/guild-invites', { token })
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

// Guild join requests (issue #242) — the applicant-initiated counterpart to
// createGuildInvite above.

export function createJoinRequest(
  token: string,
  guildId: string,
  body: CreateJoinRequestRequest,
): Promise<GuildJoinRequestResponse> {
  return request(`/guilds/${guildId}/join-requests`, { method: 'POST', body, token })
}

export function listJoinRequests(token: string, guildId: string): Promise<GuildJoinRequestResponse[]> {
  return request(`/guilds/${guildId}/join-requests`, { token })
}

// Issue #256: the caller's own pending join request for this guild, or
// `null` if they don't have one — unlike listJoinRequests above, not
// manage_members-gated, since it's only ever the caller's own data. Same
// nullable-on-200 shape getMyRecoveryStatus already uses for a
// single-resource-or-none self-scoped lookup.
export function getMyJoinRequest(
  token: string,
  guildId: string,
): Promise<GuildJoinRequestResponse | null> {
  return request(`/guilds/${guildId}/join-requests/mine`, { token })
}

export function approveJoinRequest(
  token: string,
  guildId: string,
  requestId: string,
): Promise<GuildMemberResponse> {
  return request(`/guilds/${guildId}/join-requests/${requestId}/approve`, { method: 'POST', token })
}

export function rejectJoinRequest(token: string, guildId: string, requestId: string): Promise<void> {
  return request(`/guilds/${guildId}/join-requests/${requestId}/reject`, { method: 'POST', token })
}

export function withdrawJoinRequest(token: string, guildId: string, requestId: string): Promise<void> {
  return request(`/guilds/${guildId}/join-requests/${requestId}`, { method: 'DELETE', token })
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

// Issue #253/#464 — same before/limit cursor shape as listMessages, over
// the long-window archive tier the live table's retention cap prunes
// into instead of hard-deleting. Requires *current* guild membership,
// same as listMessages (crates/server/src/guild_messages.rs::list_archive).
export function getMessageArchive(
  token: string,
  guildId: string,
  channelId: string,
  options: { before?: string; limit?: number } = {},
): Promise<ArchivedMessageResponse[]> {
  const params = new URLSearchParams()
  if (options.before) params.set('before', options.before)
  if (options.limit) params.set('limit', String(options.limit))
  const query = params.toString()
  return request(
    `/guilds/${guildId}/channels/${channelId}/messages/archive${query ? `?${query}` : ''}`,
    { token },
  )
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

// Direct/small-group conversations (issue #102/#105) —
// crates/server/src/conversations.rs.

export function listConversations(token: string): Promise<ConversationResponse[]> {
  return request('/conversations', { token })
}

// Idempotent on the final (caller included) participant set — the server
// returns the existing conversation rather than creating a duplicate, so
// this is also how the Hub "opens or starts" a conversation from a
// friend's row: just call it with that friend's id and navigate to
// whatever id comes back.
export function createConversation(
  token: string,
  body: CreateConversationRequest,
): Promise<ConversationResponse> {
  return request('/conversations', { method: 'POST', body, token })
}

export function listConversationMessages(
  token: string,
  conversationId: string,
  options: { before?: string; limit?: number } = {},
): Promise<ConversationMessageResponse[]> {
  const params = new URLSearchParams()
  if (options.before) params.set('before', options.before)
  if (options.limit) params.set('limit', String(options.limit))
  const query = params.toString()
  return request(`/conversations/${conversationId}/messages${query ? `?${query}` : ''}`, {
    token,
  })
}

export function sendConversationMessage(
  token: string,
  conversationId: string,
  body: SendConversationMessageRequest,
): Promise<ConversationMessageResponse> {
  return request(`/conversations/${conversationId}/messages`, {
    method: 'POST',
    body,
    token,
  })
}

// Guild events calendar + RSVP (issue #169) —
// crates/server/src/guild_events.rs.

export function listEvents(
  token: string,
  guildId: string,
  query: ListEventsQuery = {},
): Promise<EventResponse[]> {
  const params = new URLSearchParams()
  if (query.from) params.set('from', query.from)
  if (query.to) params.set('to', query.to)
  const queryString = params.toString()
  return request(`/guilds/${guildId}/events${queryString ? `?${queryString}` : ''}`, { token })
}

export function createEvent(
  token: string,
  guildId: string,
  body: CreateEventRequest,
): Promise<EventResponse> {
  return request(`/guilds/${guildId}/events`, { method: 'POST', body, token })
}

export function updateEvent(
  token: string,
  guildId: string,
  eventId: string,
  body: UpdateEventRequest,
): Promise<EventResponse> {
  return request(`/guilds/${guildId}/events/${eventId}`, { method: 'PATCH', body, token })
}

export function deleteEvent(
  token: string,
  guildId: string,
  eventId: string,
): Promise<{ deleted: boolean }> {
  return request(`/guilds/${guildId}/events/${eventId}`, { method: 'DELETE', token })
}

// Self-service only: always sets the caller's own RSVP, idempotent per
// (event, identity) — see crates/server/src/guild_events.rs::upsert_rsvp.
export function rsvpToEvent(
  token: string,
  guildId: string,
  eventId: string,
  body: RsvpRequest,
): Promise<RsvpResponse> {
  return request(`/guilds/${guildId}/events/${eventId}/rsvp`, { method: 'PUT', body, token })
}

// Per-member RSVP roster (issue #248) — any current guild member, no
// `manage_*` permission required. See
// crates/server/src/guild_events.rs::list_rsvps.
export function listEventRsvps(
  token: string,
  guildId: string,
  eventId: string,
): Promise<RsvpRosterEntry[]> {
  return request(`/guilds/${guildId}/events/${eventId}/rsvps`, { token })
}

// Integrator registration read (#26) + the binding/grant consent flow (#27,
// #83), matching crates/server/src/integrations.rs's #27 companion module
// field-for-field. Issue #293 made `/integrations` the server's canonical
// path for these reads (`/integrations` still works as a compatibility redirect,
// but this repo's own client calls the canonical path directly).

export function getIntegrator(token: string, slug: string): Promise<IntegratorResponse> {
  return request(`/integrations/${slug}`, { token })
}

// Issue #270's integrator directory + profile page: GET /integrations and GET
// /integrations/{slug} are both public and unauthenticated
// (crates/server/src/integrations.rs), so unlike getIntegrator above (always called
// from an already-authenticated screen) these take no bearer token at
// all — a logged-out visitor to the Hub could browse them exactly as-is
// once routing allows that (not scoped here).
export function listIntegrators(queryString: string): Promise<ListIntegratorsResponse> {
  return request(`/integrations${queryString}`)
}

export function getIntegratorPublic(slug: string): Promise<IntegratorResponse> {
  return request(`/integrations/${slug}`)
}

// Issue #261's registry-metrics endpoint — same public, unauthenticated
// visibility as getIntegrator/listIntegrators.
export function getIntegratorRegistry(slug: string): Promise<IntegratorRegistryResponse> {
  return request(`/integrations/${slug}/registry`)
}

// Issue #90's integrator profile page: an issuer's full key history, root and
// operational, valid and revoked. Same public/unauthenticated visibility
// as getIntegratorPublic/getIntegratorRegistry above.
export function listIssuerKeys(slug: string): Promise<IssuerKeyResponse[]> {
  return request(`/integrations/${slug}/keys`)
}

// User-session only — an integrator credential never grants itself anything
// (see the ticket's own invariant). Idempotent: reconnecting to an
// already-bound integrator doesn't duplicate the binding.
export function connectIntegrator(
  token: string,
  slug: string,
  body: ConnectIntegratorRequest,
): Promise<ConnectIntegratorResponse> {
  return request(`/integrations/${slug}/connect`, { method: 'POST', body, token })
}

export function revokeGrant(token: string, slug: string, capability: string): Promise<void> {
  return request(`/integrations/${slug}/grants/${capability}`, { method: 'DELETE', token })
}

export function disconnectIntegrator(token: string, slug: string): Promise<void> {
  return request(`/integrations/${slug}/connect`, { method: 'DELETE', token })
}

export function listMyConnections(token: string): Promise<MyConnectionsResponse> {
  return request('/me/connections', { token })
}

// GET /ledger/sth/latest (issues #210/#211) — the current Signed Tree Head,
// a public unauthenticated read (no `token`, matching
// crates/server/src/settlement.rs's own module doc comment). This Hub's
// first settlement/ledger API client — issue #232 adds it so the Hub can
// verify the connected server's STH against a pinned trust-anchor key
// (see apps/hub/src/network/) instead of trusting AVALON_NETWORK_ID alone.
export function getLatestSth(): Promise<SignedTreeHeadResponse> {
  return request('/ledger/sth/latest')
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

// GET /ws/messages (issue #438) — live push for guild channel and
// conversation messages, additive to listMessages'/listConversationMessages'
// point-in-time reads. Mirrors openPresenceSocket's shape (a plain browser
// WebSocket, additive subscribe, queued until `open`), one connection per
// subscription — a caller viewing a different channel/conversation closes
// this one and opens a fresh one, same as `useGuildChat`/
// `useConversationThread` already do for their poll-based predecessor.
export interface ChatSocket {
  close(): void
}

type ChatUpdate =
  | { type: 'channel_message'; data: MessageResponse }
  | { type: 'channel_message_deleted'; data: { channel_id: string; message_id: string } }
  | { type: 'conversation_message'; data: ConversationMessageResponse }

// Subscribes to one guild channel's messages. onMessage fires for every new
// message pushed; onDeleted fires with the deleted message's id when a
// moderator removes one — additive to, not a replacement for, the
// cursor-paginated GET .../messages a caller should still reconcile
// against after a reconnect.
export function openChannelMessageSocket(
  token: string,
  guildId: string,
  channelId: string,
  onMessage: (message: MessageResponse) => void,
  onDeleted: (messageId: string) => void,
): ChatSocket {
  const socket = new WebSocket(websocketUrl(`/ws/messages?token=${encodeURIComponent(token)}`))
  socket.addEventListener('open', () => {
    socket.send(
      JSON.stringify({ type: 'subscribe_channel', guild_id: guildId, channel_id: channelId }),
    )
  })
  socket.addEventListener('message', (event) => {
    const update = JSON.parse(event.data as string) as ChatUpdate
    if (update.type === 'channel_message') {
      onMessage(update.data)
    } else if (update.type === 'channel_message_deleted') {
      onDeleted(update.data.message_id)
    }
  })
  return {
    close() {
      socket.close()
    },
  }
}

// Subscribes to one conversation's messages — same shape as
// openChannelMessageSocket, minus deletion (conversations have no
// moderation-delete endpoint).
export function openConversationMessageSocket(
  token: string,
  conversationId: string,
  onMessage: (message: ConversationMessageResponse) => void,
): ChatSocket {
  const socket = new WebSocket(websocketUrl(`/ws/messages?token=${encodeURIComponent(token)}`))
  socket.addEventListener('open', () => {
    socket.send(JSON.stringify({ type: 'subscribe_conversation', conversation_id: conversationId }))
  })
  socket.addEventListener('message', (event) => {
    const update = JSON.parse(event.data as string) as ChatUpdate
    if (update.type === 'conversation_message') {
      onMessage(update.data)
    }
  })
  return {
    close() {
      socket.close()
    },
  }
}
