// Friends, blocks, presence, and discovery (issues #15/#97/#136/#204/#205)
// on AccountSession — the first-party counterpart to the integrator
// Session's own social methods. See
// crates/server/src/friends.rs/blocks.rs/presence.rs/discovery.rs.
import { AccountSession } from './core.js'

// Must match crates/protocol/src/social.rs::PresenceStatus's serde
// serialization exactly (the bare Rust enum variant names, no rename) —
// same values realtime.ts's own PresenceStatusWire (push updates) uses,
// duplicated here rather than imported to keep this file's point-in-time
// reads independent of realtime.ts's push-specific module.
export type PresenceStatus = 'Online' | 'Away' | 'DoNotDisturb' | 'Offline'

export interface Friendship {
  a: string
  b: string
  since: string
}

export interface FriendRequest {
  id: string
  from: string
  to: string
  requestedAt: string
}
interface FriendRequestWire {
  id: string
  from: string
  to: string
  requested_at: string
}

export interface Block {
  blocked: string
  createdAt: string
}
interface BlockWire {
  blocked: string
  created_at: string
}

export interface PublicProfile {
  identityId: string
  displayName: string
  avatarUrl: string | null
}
interface PublicProfileWire {
  identity_id: string
  display_name: string
  avatar_url: string | null
}

export interface DiscoveryCandidate {
  identityId: string
  displayName: string
  avatarUrl: string | null
  mutualFriends: number
  mutualGuilds: number
}
interface DiscoveryCandidateWire {
  identity_id: string
  display_name: string
  avatar_url: string | null
  mutual_friends?: number
  mutual_guilds?: number
}

export interface SearchResultIdentity {
  identityId: string
  displayName: string
  avatarUrl: string | null
}

export interface Presence {
  identityId: string
  status: PresenceStatus
  activeIn: string | null
  updatedAt: string
}
interface PresenceWire {
  identity_id: string
  status: PresenceStatus
  active_in: string | null
  updated_at: string
}

export interface HistoryEntry {
  eventId: string
  kind: string
  subject: string
  payload: unknown
  timestamp: string
}
interface HistoryEntryWire {
  event_id: string
  kind: string
  subject: string
  payload: unknown
  timestamp: string
}

export interface PublicIdentityProfile {
  identityId: string
  identityCreatedAt: string
  displayName: string
  avatarUrl: string | null
  bio: string | null
  favoriteGenres: string[]
  pronouns: string | null
  bannerUrl: string | null
  status: string | null
  links: string[]
  timezone: string | null
  themeColor: string | null
  location: string | null
}
interface PublicIdentityProfileWire {
  identity_id: string
  identity_created_at: string
  display_name: string
  avatar_url: string | null
  bio: string | null
  favorite_genres?: string[]
  pronouns: string | null
  banner_url: string | null
  status: string | null
  links?: string[]
  timezone: string | null
  theme_color: string | null
  location: string | null
}

export interface GuildAnnouncementAlert {
  messageId: string
  channelId: string
  channelName: string
  guildId: string
  author: string
  body: string
  sentAt: string
}
interface GuildAnnouncementAlertWire {
  message_id: string
  channel_id: string
  channel_name: string
  guild_id: string
  author: string
  body: string
  sent_at: string
}

declare module './core.js' {
  interface AccountSession {
    friends(): Promise<Friendship[]>
    friendRequests(): Promise<FriendRequest[]>
    createFriendRequest(to: string): Promise<FriendRequest>
    acceptFriendRequest(requestId: string): Promise<Friendship>
    declineOrWithdrawFriendRequest(requestId: string): Promise<void>
    removeFriend(identityId: string): Promise<void>
    /** `GET /friends/handle/{handle}` — exact-match handle resolution. */
    resolveHandle(handle: string): Promise<string>
    blocks(): Promise<Block[]>
    block(identityId: string): Promise<Block>
    unblock(identityId: string): Promise<void>
    discoverPeople(): Promise<DiscoveryCandidate[]>
    searchIdentities(q: string): Promise<SearchResultIdentity[]>
    profiles(ids: string[]): Promise<PublicProfile[]>
    history(): Promise<HistoryEntry[]>
    identityProfile(identityId: string): Promise<PublicIdentityProfile>
    guildAnnouncements(): Promise<GuildAnnouncementAlert[]>
    /** `PUT /me/presence` — not signature-required (high-frequency,
     * self-correcting). */
    updatePresence(status: PresenceStatus, hideActiveIn?: boolean): Promise<Presence>
    presenceOf(ids: string[]): Promise<Presence[]>
  }
}

AccountSession.prototype.friends = function (this: AccountSession): Promise<Friendship[]> {
  return this.get('/friends')
}

AccountSession.prototype.friendRequests = async function (this: AccountSession): Promise<FriendRequest[]> {
  const w = await this.get<FriendRequestWire[]>('/friends/requests')
  // Passed through as-is on a non-array body rather than crashing on
  // `.map` — same reasoning as listPasskeys/guardianRequests (passkeys.ts,
  // recovery.ts): callers that want to treat a malformed response as a
  // real failure check `Array.isArray` themselves.
  if (!Array.isArray(w)) return w as unknown as FriendRequest[]
  return w.map((r) => ({ id: r.id, from: r.from, to: r.to, requestedAt: r.requested_at }))
}

AccountSession.prototype.createFriendRequest = async function (
  this: AccountSession,
  to: string,
): Promise<FriendRequest> {
  const r = await this.post<FriendRequestWire>('/friends/requests', { to })
  return { id: r.id, from: r.from, to: r.to, requestedAt: r.requested_at }
}

AccountSession.prototype.acceptFriendRequest = function (
  this: AccountSession,
  requestId: string,
): Promise<Friendship> {
  return this.postEmpty(`/friends/requests/${requestId}/accept`)
}

AccountSession.prototype.declineOrWithdrawFriendRequest = async function (
  this: AccountSession,
  requestId: string,
): Promise<void> {
  await this.del(`/friends/requests/${requestId}`)
}

AccountSession.prototype.removeFriend = async function (this: AccountSession, identityId: string): Promise<void> {
  await this.del(`/friends/${identityId}`)
}

AccountSession.prototype.resolveHandle = async function (this: AccountSession, handle: string): Promise<string> {
  const r = await this.get<{ identity_id: string }>(`/friends/handle/${encodeURIComponent(handle)}`)
  return r.identity_id
}

AccountSession.prototype.blocks = async function (this: AccountSession): Promise<Block[]> {
  const w = await this.get<BlockWire[]>('/blocks')
  if (!Array.isArray(w)) return w as unknown as Block[]
  return w.map((b) => ({ blocked: b.blocked, createdAt: b.created_at }))
}

AccountSession.prototype.block = async function (this: AccountSession, identityId: string): Promise<Block> {
  const b = await this.post<BlockWire>('/blocks', { identity_id: identityId })
  return { blocked: b.blocked, createdAt: b.created_at }
}

AccountSession.prototype.unblock = async function (this: AccountSession, identityId: string): Promise<void> {
  await this.del(`/blocks/${identityId}`)
}

AccountSession.prototype.discoverPeople = async function (this: AccountSession): Promise<DiscoveryCandidate[]> {
  const r = await this.get<{ candidates: DiscoveryCandidateWire[] } | null>('/people/discover')
  if (!Array.isArray(r?.candidates)) return r as unknown as DiscoveryCandidate[]
  return r.candidates.map((c) => ({
    identityId: c.identity_id,
    displayName: c.display_name,
    avatarUrl: c.avatar_url,
    mutualFriends: c.mutual_friends ?? 0,
    mutualGuilds: c.mutual_guilds ?? 0,
  }))
}

AccountSession.prototype.searchIdentities = async function (
  this: AccountSession,
  q: string,
): Promise<SearchResultIdentity[]> {
  if (q.trim().length === 0) return []
  const r = await this.getQuery<{ results: PublicProfileWire[] } | null>('/identities/search', { q })
  if (!Array.isArray(r?.results)) return r as unknown as SearchResultIdentity[]
  return r.results.map((p) => ({ identityId: p.identity_id, displayName: p.display_name, avatarUrl: p.avatar_url }))
}

AccountSession.prototype.profiles = async function (this: AccountSession, ids: string[]): Promise<PublicProfile[]> {
  if (ids.length === 0) return []
  const w = await this.getQuery<PublicProfileWire[]>('/identities/profiles', { ids: ids.join(',') })
  if (!Array.isArray(w)) return w as unknown as PublicProfile[]
  return w.map((p) => ({ identityId: p.identity_id, displayName: p.display_name, avatarUrl: p.avatar_url }))
}

AccountSession.prototype.history = async function (this: AccountSession): Promise<HistoryEntry[]> {
  const w = await this.get<HistoryEntryWire[]>('/me/history')
  if (!Array.isArray(w)) return w as unknown as HistoryEntry[]
  return w.map((h) => ({ eventId: h.event_id, kind: h.kind, subject: h.subject, payload: h.payload, timestamp: h.timestamp }))
}

AccountSession.prototype.identityProfile = async function (
  this: AccountSession,
  identityId: string,
): Promise<PublicIdentityProfile> {
  const w = await this.get<PublicIdentityProfileWire>(`/identities/${identityId}/profile`)
  return {
    identityId: w.identity_id,
    identityCreatedAt: w.identity_created_at,
    displayName: w.display_name,
    avatarUrl: w.avatar_url,
    bio: w.bio,
    favoriteGenres: w.favorite_genres ?? [],
    pronouns: w.pronouns,
    bannerUrl: w.banner_url,
    status: w.status,
    links: w.links ?? [],
    timezone: w.timezone,
    themeColor: w.theme_color,
    location: w.location,
  }
}

AccountSession.prototype.guildAnnouncements = async function (
  this: AccountSession,
): Promise<GuildAnnouncementAlert[]> {
  const w = await this.get<GuildAnnouncementAlertWire[]>('/me/guild-announcements')
  if (!Array.isArray(w)) return w as unknown as GuildAnnouncementAlert[]
  return w.map((a) => ({
    messageId: a.message_id,
    channelId: a.channel_id,
    channelName: a.channel_name,
    guildId: a.guild_id,
    author: a.author,
    body: a.body,
    sentAt: a.sent_at,
  }))
}

AccountSession.prototype.updatePresence = async function (
  this: AccountSession,
  status: PresenceStatus,
  hideActiveIn?: boolean,
): Promise<Presence> {
  const w = await this.put<PresenceWire>('/me/presence', { status, hide_active_in: hideActiveIn ?? null })
  return { identityId: w.identity_id, status: w.status, activeIn: w.active_in, updatedAt: w.updated_at }
}

AccountSession.prototype.presenceOf = async function (this: AccountSession, ids: string[]): Promise<Presence[]> {
  if (ids.length === 0) return []
  const w = await this.getQuery<PresenceWire[]>('/presence', { ids: ids.join(',') })
  if (!Array.isArray(w)) return w as unknown as Presence[]
  return w.map((p) => ({ identityId: p.identity_id, status: p.status, activeIn: p.active_in, updatedAt: p.updated_at }))
}
