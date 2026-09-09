// Wire types matching crates/server/src/handlers.rs's request/response
// shapes field-for-field. The WebAuthn challenge/credential payloads are
// typed via @simplewebauthn/browser's own JSON types rather than redeclared
// here — see crypto/webauthn.ts.
import type {
  AuthenticationResponseJSON,
  PublicKeyCredentialCreationOptionsJSON,
  PublicKeyCredentialRequestOptionsJSON,
  RegistrationResponseJSON,
} from '@simplewebauthn/browser'

export interface RegisterStartRequest {
  identity_id: string
  display_name: string
}

export interface RegisterStartResponse {
  ticket_id: string
  // Server sends { publicKey: PublicKeyCredentialCreationOptionsJSON } —
  // camelCase here even though the rest of this response is snake_case,
  // because CreationChallengeResponse (crates/server/src/handlers.rs, from
  // webauthn-rs-proto) mirrors the browser's native CredentialCreationOptions
  // shape, which is camelCase per the WebAuthn spec itself.
  challenge: { publicKey: PublicKeyCredentialCreationOptionsJSON }
}

export interface RegisterFinishRequest {
  ticket_id: string
  webauthn_credential: RegistrationResponseJSON
  event_signing_public_key: string
  event_signature: string
  // A player-chosen label for the device completing this ceremony (#145) —
  // purely descriptive.
  device_label: string | null
}

export interface RegisterFinishResponse {
  identity_id: string
}

export interface SessionStartRequest {
  identity_id: string
}

export interface SessionStartResponse {
  ticket_id: string
  // Server's RequestChallengeResponse: { publicKey: ..., mediation? } — same
  // camelCase-nested-in-snake_case shape as RegisterStartResponse above.
  challenge: { publicKey: PublicKeyCredentialRequestOptionsJSON; mediation?: string }
}

export interface SessionFinishRequest {
  ticket_id: string
  credential: AuthenticationResponseJSON
}

export interface SessionFinishResponse {
  token: string
  expires_at: string
}

export interface ProfileResponse {
  identity_id: string
  identity_created_at: string
  display_name: string
  avatar_url: string | null
  // `display_name#discriminator` (issue #128) — the short handle players
  // share with each other instead of a raw identity id.
  handle: string
}

export interface UpdateProfileRequest {
  display_name?: string
  avatar_url?: string
}

// Friends/presence wire types (issue #18), matching
// crates/server/src/friends.rs and crates/server/src/presence.rs
// field-for-field — plain snake_case, unlike the WebAuthn challenge shapes
// above.

export interface FriendshipResponse {
  a: string
  b: string
  since: string
}

export interface CreateFriendRequestRequest {
  to: string
}

export interface FriendRequestResponse {
  id: string
  from: string
  to: string
  requested_at: string
}

export interface ResolveHandleResponse {
  identity_id: string
}

// GET /me/history (issue #121) — the caller's own protocol event history,
// matching crates/server/src/handlers.rs's HistoryEntryResponse.
export interface HistoryEntryResponse {
  event_id: string
  kind: string
  subject: string
  payload: unknown
  timestamp: string
}

// Device-registration / linked-device grant model (issue #135), matching
// crates/server/src/devices.rs field-for-field.

export interface RequestDeviceGrantRequest {
  requested_signing_public_key: string
  device_label: string | null
}

export interface DeviceGrantResponse {
  id: string
  status: 'pending' | 'approved' | 'denied' | 'expired'
  device_label: string | null
  requested_signing_public_key: string
  requested_at: string
  expires_at: string
}

export interface ApproveDeviceGrantRequest {
  approver_signing_key_id: string
  signature: string
}

export interface DeviceResponse {
  id: string
  label: string | null
  public_key: string
  added_at: string
  // Omitted by the server entirely when the device is still active — see
  // ProfileResponse.avatar_url's own precedent for an optional field that
  // isn't always present on the wire.
  revoked_at?: string
}

export interface RenameDeviceRequest {
  label: string
}

// Multi-passkey registration (issue #200), matching
// crates/server/src/passkeys.rs field-for-field. Distinct from the
// DeviceGrantResponse/DeviceResponse pair above: those manage
// `identity_signing_keys` (event-authorship keys, #135), these manage
// `identity_keys` (WebAuthn login credentials) — same "device with a label
// and a revoke button" shape in the UI, different underlying table and
// security property, per docs/architecture/identity.md.

export interface AddPasskeyStartResponse {
  ticket_id: string
  // Same camelCase-nested-in-snake_case shape as RegisterStartResponse
  // above — this is webauthn-rs's own CreationChallengeResponse.
  challenge: { publicKey: PublicKeyCredentialCreationOptionsJSON }
}

export interface AddPasskeyFinishRequest {
  ticket_id: string
  webauthn_credential: RegistrationResponseJSON
  label: string | null
}

export interface PasskeyResponse {
  id: string
  label: string | null
  added_at: string
}

export interface RenamePasskeyRequest {
  label: string
}

export type PresenceStatus = 'Online' | 'Away' | 'Offline'

export interface UpdatePresenceRequest {
  status: PresenceStatus
}

export interface PresenceResponse {
  identity_id: string
  status: PresenceStatus
  // Always null today — no game-side presence-publish path exists yet
  // (see #16's scope cut, and #18's own correction note on the issue).
  playing: string | null
  updated_at: string
}

// GET /identities/profiles?ids=... (issue #161) — another identity's
// public profile fields only, batched.
export interface PublicProfileResponse {
  identity_id: string
  display_name: string
  discriminator: string
  avatar_url: string | null
}

// Guild/roster/channel/message wire types (issue #24), matching
// crates/server/src/guilds.rs, crates/server/src/channels.rs, and
// crates/server/src/guild_messages.rs field-for-field.

export interface GuildResponse {
  id: string
  name: string
  tag: string
  description: string
  owner: string
  created_at: string
  member_count: number
  games: string[]
  // "invite_only" | "open" — no endpoint changes this after creation today
  // (CreateGuildRequest doesn't take it, neither does UpdateGuildRequest),
  // so every guild is "invite_only" in practice. See guilds.ts's own note.
  join_policy: string
  // Issue #206. Whether the game affinity breakdown
  // (GET /guilds/{id}/game-breakdown) is shown on this guild's public
  // profile — a manage_guild holder can always fetch the breakdown
  // regardless of this flag; it only gates exposure to everyone else.
  game_breakdown_public: boolean
}

export interface CreateGuildRequest {
  name: string
  tag: string
  description?: string
}

export interface UpdateGuildRequest {
  name?: string
  tag?: string
  description?: string
  // Issue #206. Omitted leaves it untouched.
  game_breakdown_public?: boolean
}

// GET /guilds/{id}/game-breakdown (issue #206, implementing decision #160):
// aggregated count of guild members holding an active GameBinding (#83) per
// game, computed on read — never a manager-declared association (superseded
// #20 behavior, see docs/architecture/guilds.md). No minimum-member
// threshold: every game with at least one bound member appears.
export interface GameBreakdownEntry {
  game_id: string
  game_slug: string
  game_name: string
  member_count: number
}

export interface GameBreakdownResponse {
  guild_id: string
  total_members: number
  breakdown: GameBreakdownEntry[]
}

export interface RoleResponse {
  name_index: number
  name: string
  permissions: string[]
}

export interface CreateRoleRequest {
  name: string
  permissions?: string[]
}

export interface UpdateRoleRequest {
  name?: string
  permissions?: string[]
}

export interface TransferOwnershipRequest {
  to: string
}

export interface GuildMemberResponse {
  guild_id: string
  identity_id: string
  role_index: number
  joined_at: string
}

export interface UpdateGuildMemberRequest {
  role_index: number
}

export interface CreateGuildInviteRequest {
  to: string
}

export interface GuildInviteResponse {
  id: string
  guild_id: string
  to: string
  from: string
  created_at: string
}

// Issue #154's discovery board — a distinct, narrower shape than
// GuildResponse (no `owner`/`games`/`join_policy`, matching
// crates/server/src/guilds.rs::DiscoverGuildSummary field-for-field), since
// a browse listing has no reason to fetch anything the card doesn't show.
export interface DiscoverGuildSummary {
  id: string
  name: string
  tag: string
  description: string
  recruiting: boolean
  member_count: number
  created_at: string
}

export interface DiscoverGuildsResponse {
  guilds: DiscoverGuildSummary[]
  // Present (non-null) only when another page exists — pass back as
  // `cursor=` to fetch it.
  next_cursor: string | null
}

// Query params for GET /guilds/discover — all optional, mirrors
// crates/server/src/guilds.rs::DiscoverGuildsQuery.
export interface DiscoverGuildsParams {
  q?: string
  recruiting?: boolean
  tag?: string
  game?: string
  sort?: 'newest' | 'alphabetical' | 'most_members'
  limit?: number
  cursor?: string
}

export interface MyGuildMembershipResponse {
  guild_id: string
  role_index: number
  joined_at: string
}

export interface ChannelResponse {
  id: string
  guild_id: string
  name: string
  archived: boolean
  created_at: string
}

export interface CreateChannelRequest {
  name: string
}

export interface UpdateChannelRequest {
  name: string
}

export interface MessageResponse {
  id: string
  channel_id: string
  author: string
  body: string
  sent_at: string
}

export interface SendMessageRequest {
  body: string
}

// Game registration (#26) / binding + grant consent flow (#27, #83) wire
// types, matching crates/server/src/games.rs and its #27 companion module
// field-for-field. `requested_capabilities` is a declaration only — see
// games.rs's own module doc comment — never itself a grant.

export interface GameResponse {
  id: string
  slug: string
  name: string
  developer: string
  registered_at: string
  status: string
  requested_capabilities: string[]
}

export interface ConnectGameRequest {
  capabilities: string[]
}

export interface ConnectGameResponse {
  binding_id: string
  game_id: string
  established_at: string
  granted_capabilities: string[]
}

export interface GrantResponse {
  capability: string
  granted_at: string
}

export interface GameBindingResponse {
  binding_id: string
  game_id: string
  slug: string
  name: string
  established_at: string
  grants: GrantResponse[]
}

// GET /me/connections only lists active bindings (no `ended_at` — an ended
// binding simply stops appearing), so the response is a bare array, not a
// wrapper object.
export type MyConnectionsResponse = GameBindingResponse[]
