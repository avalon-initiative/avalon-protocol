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
  // A user-chosen label for the device completing this ceremony (#145) —
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

// Issue #155's closed genre vocabulary, matching
// crates/protocol/src/identity.rs::Genre::ALL field-for-field (`as_str`'s
// snake_case wire form). Kept as a plain string union rather than derived
// from anywhere else — the Hub has no runtime dependency on the protocol
// crate — but the two lists must be changed together.
export type Genre =
  | 'action'
  | 'adventure'
  | 'rpg'
  | 'strategy'
  | 'simulation'
  | 'puzzle'
  | 'racing'
  | 'sports'
  | 'horror'
  | 'sandbox'
  | 'mmo'
  | 'shooter'
  | 'platformer'
  | 'party'

export interface ProfileResponse {
  identity_id: string
  identity_created_at: string
  display_name: string
  avatar_url: string | null
  // `display_name#discriminator` (issue #128) — the short handle users
  // share with each other instead of a raw identity id.
  handle: string
  // Issue #155's small, user-optional self-description fields — same
  // public exposure level as display_name/avatar_url above (GET /me only;
  // deliberately withheld from batch/public profile lookups server-side,
  // see crates/server/src/handlers.rs).
  bio: string | null
  favorite_genres: Genre[]
  pronouns: string | null
  // Issue #372's expanded self-description fields — same public exposure
  // level and durability contract as bio/favorite_genres/pronouns above.
  banner_url: string | null
  status: string | null
  links: string[]
  timezone: string | null
  theme_color: string | null
  // Self-described free text only — never IP-derived or geocoded.
  location: string | null
  // A self-chosen pointer to one of this identity's own current guild
  // memberships (no ticket — see
  // avalon_protocol::identity::Profile::main_guild's doc comment). `null`
  // means "not explicitly set," not "no guild" — see effective_main_guild
  // below for the resolved value to actually build a single-guild UI
  // around.
  main_guild: string | null
  // main_guild if explicitly set, otherwise the guild this identity joined
  // earliest, computed server-side at read time and never stored. `null`
  // only when the identity has no guild memberships at all.
  effective_main_guild: string | null
  // Issue #205's opt-in global search toggle — true means this identity
  // currently matches GET /identities/search. Off by default for every
  // identity; drives the "you are currently publicly searchable" indicator
  // on Profile.vue.
  discoverable: boolean
  // Issue #87 — who can see this identity's presence status. One of
  // "public" | "authenticated_only" | "friends" | "guild_members" |
  // "private", same vocabulary GuildResponse.roster_visibility uses.
  presence_visibility: string
}

export interface UpdateProfileRequest {
  display_name?: string
  avatar_url?: string
  // Three-state fields (issue #155): omitted leaves the existing value
  // untouched, "" clears it, a non-empty string validates then sets it —
  // same convention avatar_url already uses.
  bio?: string
  pronouns?: string
  // Two states, not three: omitted (untouched) or a full replacement list,
  // including [] to clear it.
  favorite_genres?: Genre[]
  // Issue #372's expanded self-description fields. banner_url/status/
  // timezone/theme_color/location are three-state, same convention as
  // UpdateProfileRequest.bio). links: omit to leave untouched, any array
  // (including []) always fully replaces the stored list.
  banner_url?: string
  status?: string
  links?: string[]
  timezone?: string
  theme_color?: string
  location?: string
  // Three states, same as bio: omitted (untouched), "" (clear), or a guild
  // id the caller must currently be a member of — rejected otherwise, not
  // silently ignored. Not free text; the Hub only ever sends an id from
  // the caller's own GET /me/guilds list.
  main_guild?: string
  // Issue #205. Omitted leaves the existing preference untouched.
  discoverable?: boolean
  // Issue #87. Omitted leaves it untouched.
  presence_visibility?: string
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

// Issue #97 — matches crates/server/src/blocks.rs field-for-field.
// Blocking is unilateral, private application state (never a protocol
// event, never durable history) — GET /blocks only ever returns the
// caller's own outgoing blocks, never who has blocked the caller.
export interface CreateBlockRequest {
  identity_id: string
}

export interface BlockResponse {
  blocked: string
  created_at: string
}

export interface BlockListEntry {
  blocked: string
  created_at: string
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

// GET /me/guild-announcements (issue #280) — recent posts to any
// announcement-only channel in any guild the caller currently belongs to,
// matching crates/server/src/guild_messages.rs's GuildAnnouncementAlert.
export interface GuildAnnouncementAlert {
  message_id: string
  channel_id: string
  channel_name: string
  guild_id: string
  author: string
  body: string
  sent_at: string
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

// Issue #443: identities that currently name the caller as one of their
// recovery guardians.
export interface GuardianOfSummary {
  identity_id: string
  display_name: string
  discriminator: string
  added_at: string
}

export type PresenceStatus = 'Online' | 'Away' | 'DoNotDisturb' | 'Offline'

export interface UpdatePresenceRequest {
  status: PresenceStatus
}

export interface PresenceResponse {
  identity_id: string
  status: PresenceStatus
  // Always null today — no integrator-side presence-publish path exists yet
  // (see #16's scope cut, and #18's own correction note on the issue).
  active_in: string | null
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

// GET /identities/:id/profile (issue #403) — a single identity's full
// self-description fields, same exposure level as ProfileResponse (GET
// /me) above, minus `discoverable` (that's the viewed identity's own
// search-visibility setting, not something the viewer needs).
export interface PublicIdentityProfileResponse {
  identity_id: string
  identity_created_at: string
  display_name: string
  avatar_url: string | null
  handle: string
  bio: string | null
  favorite_genres: Genre[]
  pronouns: string | null
  banner_url: string | null
  status: string | null
  links: string[]
  timezone: string | null
  theme_color: string | null
  location: string | null
  main_guild: string | null
  effective_main_guild: string | null
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
  integrators: string[]
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
  // Issue #449. Independent of recruiting: gates roster (and, once event
  // visibility lands, public event) exposure to any authenticated
  // identity, regardless of recruiting status.
  public: boolean
  // Issue #206. Whether the integrator affinity breakdown
  // (GET /guilds/{id}/integrator-breakdown) is shown on this guild's public
  // profile — a manage_guild holder can always fetch the breakdown
  // regardless of this flag; it only gates exposure to everyone else.
  game_breakdown_public: boolean
  // Issue #207. The guild's curated top-5 favorite integrators, in display
  // order — always part of the public profile (unlike the full
  // breakdown, which stays behind game_breakdown_public).
  favorite_games: FavoriteGameEntry[]
  // Issue #87. Who can see this guild's member list, independent of the
  // recruiting/public overrides (#449/#455) that can widen it further —
  // one of "public" | "authenticated_only" | "friends" | "guild_members" |
  // "private".
  roster_visibility: string
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
  // Issue #449. Omitted leaves it untouched. Independent of recruiting.
  public?: boolean
  // Issue #206. Omitted leaves it untouched.
  game_breakdown_public?: boolean
  // "invite_only" or "open" — omitted leaves it untouched. "open" lets any
  // authenticated identity join instantly via POST /guilds/{id}/join,
  // bypassing the invite and join-request/approval flows entirely.
  join_policy?: 'invite_only' | 'open'
  // Issue #87. Omitted leaves it untouched. The recruiting/public overrides
  // (#449/#455) can still widen roster exposure beyond whatever this is
  // set to; this only controls the underlying baseline.
  roster_visibility?: string
}

// GET /guilds/{id}/integrator-breakdown (issue #206, implementing decision #160):
// aggregated count of guild members holding an active IntegratorBinding (#83) per
// integrator, computed on read — never a manager-declared association (superseded
// #20 behavior, see docs/architecture/guilds.md). No minimum-member
// threshold: every integrator with at least one bound member appears.
export interface GameBreakdownEntry {
  integrator_id: string
  integrator_slug: string
  integrator_name: string
  member_count: number
}

export interface GameBreakdownResponse {
  guild_id: string
  total_members: number
  breakdown: GameBreakdownEntry[]
}

// GET /guilds/{id}/favorite-integrators and PUT /guilds/{id}/favorite-integrators
// (issue #207, implementing decision #160): a manage_guild-curated, capped
// (5), ordered pin list drawn only from integrators that already appear in the
// affinity breakdown above. `stale` is computed live against the same
// binding data on every read — a stale pin is never auto-removed (see
// crates/server/src/guilds.rs's module doc comment), just flagged so a
// manage_guild holder can choose to unpin it.
export interface FavoriteGameEntry {
  integrator_id: string
  integrator_slug: string
  integrator_name: string
  position: number
  stale: boolean
}

export interface FavoriteGamesResponse {
  guild_id: string
  favorites: FavoriteGameEntry[]
}

export interface SetFavoriteGamesRequest {
  // Full desired ordered list of pinned integrator ids — always a full replace,
  // same convention UpdateGuildRequest's `links` field uses server-side.
  integrator_ids: string[]
}

// Issue #152's closed badge vocabulary — matches
// avalon_protocol::guilds::{RoleBadgeIcon,RoleBadgeColor}::ALL exactly.
export type RoleBadgeIconId = 'shield' | 'crown' | 'star' | 'sword' | 'wrench' | 'heart' | 'flag' | 'bolt'
export type RoleBadgeColorId = 'gray' | 'red' | 'orange' | 'gold' | 'green' | 'blue' | 'purple'

export interface RoleBadge {
  icon: RoleBadgeIconId
  color: RoleBadgeColorId
}

export interface RoleResponse {
  name_index: number
  name: string
  permissions: string[]
  // Issue #152. Empty string when unset.
  description: string
  // Issue #152. RoleBadge::DEFAULT (shield/gray) when never explicitly set.
  badge: RoleBadge
}

export interface CreateRoleRequest {
  name: string
  permissions?: string[]
  // Issue #152. Omitted defaults to "" server-side.
  description?: string
  // Issue #152. Omitted defaults to RoleBadge::DEFAULT server-side.
  badge?: RoleBadge
}

export interface UpdateRoleRequest {
  name?: string
  permissions?: string[]
  // Issue #152. Omitted leaves it untouched.
  description?: string
  // Issue #152. Omitted leaves it untouched.
  badge?: RoleBadge
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

// Issue #442: the invitee's own view of a pending invite — includes the
// guild's name since the invitee (unlike the sender) may not have visited
// the guild yet.
export interface MyGuildInviteResponse {
  id: string
  guild_id: string
  guild_name: string
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
// GuildResponse (no `owner`/`integrators`/`join_policy`, matching
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
  integrator?: string
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
  // Issue #276: a short line describing what the channel is for, shown in
  // the channel header. `null`/unset means no topic.
  topic: string | null
}

export interface CreateChannelRequest {
  name: string
}

export interface UpdateChannelRequest {
  name: string
  // Issue #250. Omitted leaves the existing value untouched.
  announcement_only?: boolean
  // Issue #276. Omitted leaves the existing value untouched; an empty
  // string clears it.
  topic?: string
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

// Issue #253/#464 — same shape as MessageResponse plus archived_at, over
// guild_messages_archive instead of the live table. Matches
// crates/server/src/guild_messages.rs::ArchivedMessageResponse.
export interface ArchivedMessageResponse {
  id: string
  channel_id: string
  author: string
  body: string
  sent_at: string
  archived_at: string
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

// Integrator registration (#26) / binding + grant consent flow (#27, #83) wire
// types, matching crates/server/src/integrations.rs and its #27 companion module
// field-for-field. `requested_capabilities` is a declaration only — see
// integrators.rs's own module doc comment — never itself a grant.

// What kind of integrator a registration is (#282). Additive on the wire.
export type IntegratorCategory = 'game' | 'app' | 'service'

export interface IntegratorResponse {
  id: string
  slug: string
  name: string
  owner_name: string
  registered_at: string
  status: string
  category: IntegratorCategory
  requested_capabilities: string[]
}

// Issue #270's integrator directory board — a distinct, narrower shape than
// IntegratorResponse (no `requested_capabilities`, matching
// crates/server/src/integrations.rs::IntegratorSummary field-for-field), since a
// directory card has no reason to fetch a field it doesn't show — same
// reasoning DiscoverGuildSummary above already documents for guilds.
export interface IntegratorSummary {
  id: string
  slug: string
  name: string
  owner_name: string
  registered_at: string
  status: string
  category: IntegratorCategory
}

export interface ListIntegratorsResponse {
  integrators: IntegratorSummary[]
  // Present (non-null) only when another page exists — pass back as
  // `cursor=` to fetch it.
  next_cursor: string | null
}

// Query params for GET /integrations — all optional, mirrors
// crates/server/src/integrations.rs::ListIntegratorsQuery.
export interface ListIntegratorsParams {
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

// GET /integrations/{slug}/registry's response (issue #261), matching
// crates/server/src/registry.rs::IntegratorRegistryResponse field-for-field.
export interface IntegratorRegistryResponse {
  players: MetricResponse
  total_players_ever: MetricResponse
  achievements_issued: MetricResponse
  achievements_revoked: MetricResponse
  unique_achievement_holders: MetricResponse
}

// GET /integrations/{slug}/keys's response (issue #90): an issuer's full key
// history, oldest first — public and unauthenticated, matching
// crates/server/src/integrations.rs::IssuerKeyResponse field-for-field.
export interface IssuerKeyResponse {
  key_id: string
  algorithm: string
  role: 'root' | 'operational'
  valid_from: string
  valid_until: string | null
  revoked_at: string | null
}

export interface ConnectIntegratorRequest {
  capabilities: string[]
}

export interface ConnectIntegratorResponse {
  binding_id: string
  integrator_id: string
  established_at: string
  granted_capabilities: string[]
}

export interface GrantResponse {
  capability: string
  granted_at: string
}

export interface IntegratorBindingResponse {
  binding_id: string
  integrator_id: string
  slug: string
  name: string
  established_at: string
  grants: GrantResponse[]
}

// GET /me/connections only lists active bindings (no `ended_at` — an ended
// binding simply stops appearing), so the response is a bare array, not a
// wrapper object.
export type MyConnectionsResponse = IntegratorBindingResponse[]

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
  // Issue #448. false (the default) keeps this event member-only even in
  // a public guild (GuildResponse.public).
  public: boolean
  // Issue #463. The caller's own RSVP status, or null if they haven't
  // responded — never another member's. Lets AvalonRsvpControl pre-select
  // correctly without a separate roster fetch.
  my_rsvp: RsvpStatusValue | null
}

export interface CreateEventRequest {
  channel_id?: string | null
  title: string
  description?: string | null
  starts_at: string
  ends_at?: string | null
  // Issue #448. Omitted defaults to false.
  public?: boolean
}

export interface UpdateEventRequest {
  channel_id?: string | null
  title: string
  description?: string | null
  starts_at: string
  ends_at?: string | null
  // Issue #448. Always resent, full replace like the rest of this request.
  public?: boolean
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

// GET /me/achievements's response envelope (issue #377) — matching
// crates/server/src/attestations.rs::ListMyAchievementsResponse. Wraps
// what used to be a bare array so the endpoint could gain cursor
// pagination without silently truncating a caller that isn't ready for
// it. `next_cursor` isn't consumed by the Hub yet — see
// api/achievements.ts::listMyAchievements, which requests the server's
// max page size instead of wiring up real "load more" UI, matching the
// same scope decision the Rust SDK made for #377.
export interface ListMyAchievementsResponse {
  achievements: AttestationResponse[]
  next_cursor: string | null
}

// GET /integrations/{slug}/achievements and GET /integrations/{slug}/milestones
// (#31/#324/#325), matching crates/server/src/achievements.rs's
// AchievementDefinitionResponse field-for-field. `id` is the definition's
// GlobalId string ("game:<slug>:achievement:<key>" or the milestone
// equivalent) — the same string AttestationResponse.achievement carries,
// which is how achievements.ts resolves a display name for a claim.
export interface AchievementDefinitionResponse {
  id: string
  integrator_id: string
  key: string
  name: string
  description: string
  schema?: string
  // Always populated server-side (falls back to the hardcoded default,
  // "trophy", when the definition has neither field set — issue #332).
  icon: string
  icon_url?: string
  version: number
  created_at: string
  updated_at: string
  retired: boolean
  retired_at?: string
}
