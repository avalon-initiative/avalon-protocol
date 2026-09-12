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
  // Issue #205's opt-in global search toggle — true means this identity
  // currently matches GET /identities/search. Off by default for every
  // identity; drives the "you are currently publicly searchable" indicator
  // on Profile.vue.
  discoverable: boolean
}

export interface UpdateProfileRequest {
  display_name?: string
  avatar_url?: string
  // Issue #205. Omitted leaves the existing preference untouched.
  discoverable?: boolean
}

// GET /identities/search?q=&limit= (issue #205) — the opt-in counterpart to
// GET /people/discover, matching crates/server/src/discovery.rs's
// SearchIdentitiesResponse. Only ever returns identities with
// discoverable = true.
export interface SearchResultIdentity {
  identity_id: string
  display_name: string
  discriminator: string
  avatar_url: string | null
}

export interface SearchIdentitiesResponse {
  results: SearchResultIdentity[]
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

// GET /people/discover (issue #204) — matches
// crates/server/src/discovery.rs's DiscoverPeopleResponse. No request
// params: the only input is the caller's own session, never a search term
// (see that module's own doc comment).
export interface DiscoveryCandidate {
  identity_id: string
}

export interface DiscoverPeopleResponse {
  candidates: DiscoveryCandidate[]
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

// Cross-device pairing (issue #307), matching
// crates/server/src/device_pairing.rs field-for-field. Bootstraps a session
// for a WebAuthn-incapable client — distinct from the device grant model
// above, which adds a signing key to an identity that's already
// authenticated somewhere.

export interface StartPairingResponse {
  device_code: string
  user_code: string
  verification_uri: string
  expires_in: number
  poll_interval: number
}

export interface PollPairingResponse {
  status: 'pending' | 'slow_down' | 'denied' | 'expired' | 'approved'
  token?: string
  expires_at?: string
}

export interface UserCodeRequest {
  user_code: string
}

export interface ResolvePairingResponse {
  status: 'approved' | 'denied'
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

// Social recovery (issue #201), matching crates/server/src/recovery.rs
// field-for-field.

export interface SetGuardiansRequest {
  guardian_ids: string[]
  threshold: number
}

export interface GuardianSettingsResponse {
  guardian_ids: string[]
  threshold: number
  updated_at: string | null
}

export interface RecoveryStartRequest {
  identity_id: string
  device_label: string | null
}

export interface RecoveryStartResponse {
  ticket_id: string
  challenge: { publicKey: PublicKeyCredentialCreationOptionsJSON }
}

export interface RecoveryFinishRequest {
  ticket_id: string
  webauthn_credential: RegistrationResponseJSON
}

export type RecoveryStatus = 'pending_approvals' | 'delay' | 'completed' | 'cancelled'

export interface RecoveryRequestResponse {
  id: string
  identity_id: string
  status: RecoveryStatus
  threshold: number
  approvals_count: number
  requested_at: string
  delay_ends_at: string | null
}

export interface CancelRecoveryRequest {
  reason: string | null
}

export interface GuardianRequestSummary {
  request: RecoveryRequestResponse
  already_approved: boolean
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

// One entry in GuildResponse.links (issue #153), matching
// crates/protocol/src/guilds.rs::GuildLink field-for-field.
export interface GuildLink {
  label: string
  url: string
}

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
  // Issue #153, all four below. motd/banner are null when unset;
  // recruiting gates the "recruiting only" discovery filter (#154) and
  // whether this guild shows up under a recruiting=false lookup for
  // strangers at all (see build_discover_query's membership gating).
  motd: string | null
  banner: string | null
  // Issue #246. Same shape as banner: a small badge image, null when unset.
  icon: string | null
  links: GuildLink[]
  recruiting: boolean
  // Issue #206. Whether the game affinity breakdown
  // (GET /guilds/{id}/game-breakdown) is shown on this guild's public
  // profile — a manage_guild holder can always fetch the breakdown
  // regardless of this flag; it only gates exposure to everyone else.
  game_breakdown_public: boolean
  // Issue #207. The guild's curated top-5 favorite games, in display
  // order — always part of the public profile (unlike the full
  // breakdown, which stays behind game_breakdown_public).
  favorite_games: FavoriteGameEntry[]
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
  // Issue #153, all four below. motd/banner: omit to leave untouched,
  // "" to clear, non-empty to set (three-state, same convention as
  // UpdateProfileRequest.bio). links: omit to leave untouched, any array
  // (including []) to fully replace the stored list — never a per-entry
  // patch. recruiting: omit to leave untouched.
  motd?: string
  banner?: string
  // Issue #246. Same three-state convention as banner: omit to leave
  // untouched, "" to clear, non-empty to set.
  icon?: string
  links?: GuildLink[]
  recruiting?: boolean
  // Issue #206. Omitted leaves it untouched.
  game_breakdown_public?: boolean
  // "invite_only" or "open" — omitted leaves it untouched. "open" lets any
  // authenticated identity join instantly via POST /guilds/{id}/join,
  // bypassing the invite and join-request/approval flows entirely.
  join_policy?: 'invite_only' | 'open'
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

// GET /guilds/{id}/favorite-games and PUT /guilds/{id}/favorite-games
// (issue #207, implementing decision #160): a manage_guild-curated, capped
// (5), ordered pin list drawn only from games that already appear in the
// affinity breakdown above. `stale` is computed live against the same
// binding data on every read — a stale pin is never auto-removed (see
// crates/server/src/guilds.rs's module doc comment), just flagged so a
// manage_guild holder can choose to unpin it.
export interface FavoriteGameEntry {
  game_id: string
  game_slug: string
  game_name: string
  position: number
  stale: boolean
}

export interface FavoriteGamesResponse {
  guild_id: string
  favorites: FavoriteGameEntry[]
}

export interface SetFavoriteGamesRequest {
  // Full desired ordered list of pinned game ids — always a full replace,
  // same convention UpdateGuildRequest's `links` field uses server-side.
  game_ids: string[]
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

// Issue #242's applicant-initiated counterpart to GuildInviteResponse.
export interface CreateJoinRequestRequest {
  message?: string
}

export interface GuildJoinRequestResponse {
  id: string
  guild_id: string
  applicant: string
  message: string | null
  status: 'pending' | 'approved' | 'rejected' | 'withdrawn'
  created_at: string
  decided_at: string | null
  decided_by: string | null
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
  // Issue #258: same already-public fields GuildResponse carries
  // (#153/#246) — null when unset.
  banner: string | null
  icon: string | null
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
  // Issue #250: when true, posting requires the `channel_post` permission
  // (resolved per-channel via the override layer below) instead of plain
  // membership.
  announcement_only: boolean
}

export interface CreateChannelRequest {
  name: string
}

export interface UpdateChannelRequest {
  name: string
  // Issue #250. Omitted leaves the existing value untouched.
  announcement_only?: boolean
}

// Per-resource guild permission overrides (issue #250), matching
// crates/server/src/guilds.rs::PermissionOverrideResponse /
// SetPermissionOverrideRequest field-for-field.
export type GuildResourceKind = 'channel' | 'event'

export interface PermissionOverrideResponse {
  id: string
  role_index: number
  resource_kind: GuildResourceKind
  resource_id: string
  permission: string
  allow: boolean
}

export interface SetPermissionOverrideRequest {
  role_index: number
  resource_kind: GuildResourceKind
  resource_id: string
  permission: string
  allow: boolean
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

// Direct/small-group conversations (issue #102/#105), matching
// crates/server/src/conversations.rs field-for-field. Deliberately its own
// shapes rather than reusing GuildResponse/MessageResponse above — a
// conversation has no guild context, and its messages carry
// `conversation_id`, not `channel_id`.

export interface ConversationResponse {
  id: string
  participants: string[]
}

export interface CreateConversationRequest {
  participants: string[]
}

export interface ConversationMessageResponse {
  id: string
  conversation_id: string
  author: string
  body: string
  sent_at: string
}

export interface SendConversationMessageRequest {
  body: string
  client_entry_id?: string
}

// Game registration (#26) / binding + grant consent flow (#27, #83) wire
// types, matching crates/server/src/games.rs and its #27 companion module
// field-for-field. `requested_capabilities` is a declaration only — see
// games.rs's own module doc comment — never itself a grant.

// What kind of integrator a registration is (#282). Additive on the wire.
export type IntegratorCategory = 'game' | 'app' | 'service'

export interface GameResponse {
  id: string
  slug: string
  name: string
  developer: string
  registered_at: string
  status: string
  category: IntegratorCategory
  requested_capabilities: string[]
}

// Issue #270's game directory board — a distinct, narrower shape than
// GameResponse (no `requested_capabilities`, matching
// crates/server/src/games.rs::GameSummary field-for-field), since a
// directory card has no reason to fetch a field it doesn't show — same
// reasoning DiscoverGuildSummary above already documents for guilds.
export interface GameSummary {
  id: string
  slug: string
  name: string
  developer: string
  registered_at: string
  status: string
  category: IntegratorCategory
}

export interface ListGamesResponse {
  games: GameSummary[]
  // Present (non-null) only when another page exists — pass back as
  // `cursor=` to fetch it.
  next_cursor: string | null
}

// Query params for GET /games — all optional, mirrors
// crates/server/src/games.rs::ListGamesQuery.
export interface ListGamesParams {
  q?: string
  sort?: 'newest' | 'name'
  limit?: number
  cursor?: string
}

// One registry metric (issue #261), matching
// crates/server/src/registry.rs::MetricResponse field-for-field — never
// rendered as a bare `value` anywhere in the Hub (issue #270's own
// invariant); see AvalonMetricTile in @avalon/ui.
export interface MetricResponse {
  value: number
  definition: string
  class: string
}

// GET /games/{slug}/registry's response (issue #261), matching
// crates/server/src/registry.rs::GameRegistryResponse field-for-field.
export interface GameRegistryResponse {
  players: MetricResponse
  total_players_ever: MetricResponse
  achievements_issued: MetricResponse
  achievements_revoked: MetricResponse
  unique_achievement_holders: MetricResponse
}

// GET /games/{slug}/keys's response (issue #90): an issuer's full key
// history, oldest first — public and unauthenticated, matching
// crates/server/src/games.rs::IssuerKeyResponse field-for-field.
export interface IssuerKeyResponse {
  key_id: string
  algorithm: string
  role: 'root' | 'operational'
  valid_from: string
  valid_until: string | null
  revoked_at: string | null
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

// Settlement / transparency-log reads (issues #210/#211/#232), matching
// `crates/server/src/settlement.rs::SignedTreeHeadResponse` field-for-field.
// `tree_size` and `created_at`'s unix-seconds form both feed
// `apps/hub/src/network/sthMessage.ts`'s byte-for-byte reconstruction of
// `crates/chain/src/sth.rs::signing_message` — see that module.
export interface SignedTreeHeadResponse {
  tree_size: number
  // Lowercase hex-encoded RFC 6962 Merkle Tree Hash.
  root_hash: string
  network_id: string
  signing_key_id: string
  // Lowercase hex-encoded Ed25519 signature (64 bytes).
  signature: string
  // RFC 3339 (`time::serde::rfc3339` on the server side).
  created_at: string
}

// Guild events calendar + RSVP (issue #169). See
// `crates/server/src/guild_events.rs` and `docs/architecture/guilds.md`'s
// "Guild events calendar + RSVP" section for the durability call: no
// `guild_events`/`guild_event_rsvps` row is protocol history, both are
// plain projections, same "hot state, not history" posture chat messages
// already have.

export interface RsvpCounts {
  going: number
  maybe: number
  not_going: number
}

export interface EventResponse {
  id: string
  guild_id: string
  channel_id: string | null
  title: string
  description: string | null
  starts_at: string
  ends_at: string | null
  created_by: string
  created_at: string
  rsvp_counts: RsvpCounts
}

export interface CreateEventRequest {
  channel_id?: string | null
  title: string
  description?: string | null
  starts_at: string
  ends_at?: string | null
}

export interface UpdateEventRequest {
  channel_id?: string | null
  title: string
  description?: string | null
  starts_at: string
  ends_at?: string | null
}

export type RsvpStatusValue = 'going' | 'maybe' | 'not_going'

export interface RsvpRequest {
  status: RsvpStatusValue
}

export interface RsvpResponse {
  event_id: string
  identity_id: string
  status: RsvpStatusValue
  responded_at: string
}

export interface ListEventsQuery {
  from?: string
  to?: string
}

// Per-member RSVP roster (issue #248) — GET /guilds/{id}/events/{eid}/rsvps.
// Every `guild_event_rsvps` row for the event, unaggregated (the roster
// behind `EventResponse.rsvp_counts`).
export interface RsvpRosterEntry {
  identity_id: string
  status: RsvpStatusValue
  responded_at: string
}

// GET /me/achievements (issue #34/#35), matching
// crates/server/src/attestations.rs's AttestationReadResponse and its
// nested types field-for-field. `authenticity`/`validity` are internally
// tagged on `status` (serde's `tag = "status", rename_all = "snake_case"`)
// with the variant-specific field (`key_id`/`reason`) alongside it in the
// same object.
export interface AttestationAuthenticityResponse {
  status: 'authentic' | 'not_authentic'
  key_id?: string
  reason?: string
}

export interface AttestationValidityResponse {
  status: 'valid' | 'invalid'
  reason?: string
}

export interface AttestationHistoryEntryResponse {
  event: string
  at: string
  reason_code?: string
  reason?: string
}

export interface AttestationProofResponse {
  key_id: string
  algorithm: string
}

export interface AttestationResponse {
  id: string
  issuer: string
  subject: string
  achievement: string
  issued_at: string
  proof: AttestationProofResponse
  authenticity: AttestationAuthenticityResponse
  validity: AttestationValidityResponse
  history: AttestationHistoryEntryResponse[]
  // Deliberately no `recognition` field — see
  // crates/server/src/attestations.rs's own module doc comment (ADR #76:
  // recognition is the consumer's own policy call, never the server's).
}

// GET /games/{slug}/achievements and GET /integrations/{slug}/milestones
// (#31/#324/#325), matching crates/server/src/achievements.rs's
// AchievementDefinitionResponse field-for-field. `id` is the definition's
// GlobalId string ("game:<slug>:achievement:<key>" or the milestone
// equivalent) — the same string AttestationResponse.achievement carries,
// which is how achievements.ts resolves a display name for a claim.
export interface AchievementDefinitionResponse {
  id: string
  game_id: string
  key: string
  name: string
  description: string
  schema?: string
  version: number
  created_at: string
  updated_at: string
  retired: boolean
  retired_at?: string
}
