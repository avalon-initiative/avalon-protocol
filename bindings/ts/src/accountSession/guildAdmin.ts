// Full guild administration (issues #20/#21/#22/#152/#153/#169/#242/#250/
// #442) on AccountSession — creation, roles, per-resource permission
// overrides, ownership transfer, membership, invites, join requests,
// channels, chat, and events, as an identity acting with its own authority.
// See crates/server/src/guilds.rs/channels.rs/guild_messages.rs/
// guild_events.rs.
import { AccountSession } from './core.js'
import type { components } from '../generated.js'

export type GuildLink = components['schemas']['GuildLink']

export interface Guild {
  id: string
  name: string
  tag: string
  description: string
  owner: string
  createdAt: string
  memberCount: number
  integrators: string[]
  joinPolicy: string
  motd: string | null
  banner: string | null
  icon: string | null
  links: GuildLink[]
  recruiting: boolean
  public: boolean
  // Issue #206 — whether the integrator affinity breakdown
  // (getGameBreakdown) is shown on this guild's public profile.
  gameBreakdownPublic: boolean
  // Issue #207 — the guild's curated top-5 favorite integrators, always
  // part of the public profile regardless of gameBreakdownPublic. Same
  // shape favoriteGames(guildId) returns.
  favoriteGames: FavoriteGameEntry[]
  // Issue #87 — "public" | "guild_members" | "private".
  rosterVisibility: string
}
type GuildWire = components['schemas']['GuildResponse']
function guildFromWire(w: GuildWire): Guild {
  return {
    id: w.id,
    name: w.name,
    tag: w.tag,
    description: w.description,
    owner: w.owner,
    createdAt: w.created_at,
    memberCount: w.member_count,
    integrators: w.integrators ?? [],
    joinPolicy: w.join_policy,
    motd: w.motd ?? null,
    banner: w.banner ?? null,
    icon: w.icon ?? null,
    links: w.links ?? [],
    recruiting: w.recruiting ?? false,
    public: w.public ?? false,
    gameBreakdownPublic: w.game_breakdown_public ?? false,
    favoriteGames: (w.favorite_games ?? []).map(favoriteGameEntryFromWire),
    rosterVisibility: w.roster_visibility ?? 'guild_members',
  }
}

export interface DiscoverGuildSummary {
  id: string
  name: string
  tag: string
  description: string
  recruiting: boolean
  memberCount: number
  createdAt: string
  banner: string | null
  icon: string | null
}

export interface DiscoverGuildsPage {
  guilds: DiscoverGuildSummary[]
  nextCursor: string | null
}
type DiscoverGuildsPageWire = components['schemas']['DiscoverGuildsResponse']

export interface FavoriteGameEntry {
  integratorId: string
  integratorSlug: string
  integratorName: string
  position: number
  stale: boolean
}
type FavoriteGameEntryWire = components['schemas']['FavoriteGameEntry']
function favoriteGameEntryFromWire(f: FavoriteGameEntryWire): FavoriteGameEntry {
  return {
    integratorId: f.integrator_id,
    integratorSlug: f.integrator_slug,
    integratorName: f.integrator_name,
    position: f.position,
    stale: f.stale,
  }
}

export interface FavoriteGames {
  guildId: string
  favorites: FavoriteGameEntry[]
}
type FavoriteGamesWire = components['schemas']['FavoriteGamesResponse']

export interface RoleBadge {
  icon: string | null
  color: string | null
}

/** The write-side shape createRole/updateRole take — unlike the read-side
 * RoleBadge, both fields are required non-null ids the server validates
 * against RoleBadgeIcon/RoleBadgeColor, rejecting an unrecognized one
 * rather than silently dropping it. */
export interface RoleBadgeUpdate {
  icon: string
  color: string
}

export interface Role {
  nameIndex: number
  name: string
  permissions: string[]
  description: string
  badge: RoleBadge
}
type RoleWire = components['schemas']['RoleResponse']
function roleFromWire(w: RoleWire): Role {
  return {
    nameIndex: w.name_index,
    name: w.name,
    permissions: w.permissions,
    description: w.description,
    badge: { icon: w.badge.icon, color: w.badge.color },
  }
}

export interface PermissionOverride {
  id: string
  roleIndex: number
  resourceKind: string
  resourceId: string
  permission: string
  allow: boolean
}
type PermissionOverrideWire = components['schemas']['PermissionOverrideResponse']
function overrideFromWire(w: PermissionOverrideWire): PermissionOverride {
  return { id: w.id, roleIndex: w.role_index, resourceKind: w.resource_kind, resourceId: w.resource_id, permission: w.permission, allow: w.allow }
}

export interface GuildMember {
  guildId: string
  identityId: string
  roleIndex: number
  joinedAt: string
}
type GuildMemberWire = components['schemas']['GuildMemberResponse']
function memberFromWire(w: GuildMemberWire): GuildMember {
  return { guildId: w.guild_id, identityId: w.identity_id, roleIndex: w.role_index, joinedAt: w.joined_at }
}

export interface MyGuildMembership {
  guildId: string
  roleIndex: number
  joinedAt: string
}

export interface GuildInvite {
  id: string
  guildId: string
  to: string
  from: string
  createdAt: string
}
type GuildInviteWire = components['schemas']['GuildInviteResponse']

export interface MyGuildInvite {
  id: string
  guildId: string
  guildName: string
  from: string
  createdAt: string
}
type MyGuildInviteWire = components['schemas']['MyGuildInviteResponse']

export interface GuildJoinRequest {
  id: string
  guildId: string
  applicant: string
  message: string | null
  status: string
  createdAt: string
  decidedAt: string | null
  decidedBy: string | null
}
type GuildJoinRequestWire = components['schemas']['GuildJoinRequestResponse']
function joinRequestFromWire(w: GuildJoinRequestWire): GuildJoinRequest {
  return {
    id: w.id,
    guildId: w.guild_id,
    applicant: w.applicant,
    message: w.message ?? null,
    status: w.status,
    createdAt: w.created_at,
    decidedAt: w.decided_at ?? null,
    decidedBy: w.decided_by ?? null,
  }
}

export interface GuildChannel {
  id: string
  guildId: string
  name: string
  archived: boolean
  createdAt: string
  announcementOnly: boolean
  topic: string | null
  public: boolean
}
type GuildChannelWire = components['schemas']['ChannelResponse']
function channelFromWire(w: GuildChannelWire): GuildChannel {
  return {
    id: w.id,
    guildId: w.guild_id,
    name: w.name,
    archived: w.archived,
    createdAt: w.created_at,
    announcementOnly: w.announcement_only,
    topic: w.topic ?? null,
    public: w.public ?? false,
  }
}

export interface GuildMessage {
  id: string
  channelId: string
  author: string
  body: string
  sentAt: string
}
type GuildMessageWire = components['schemas']['MessageResponse']
function guildMessageFromWire(w: GuildMessageWire): GuildMessage {
  return { id: w.id, channelId: w.channel_id, author: w.author, body: w.body, sentAt: w.sent_at }
}

export interface RsvpCounts {
  going: number
  maybe: number
  notGoing: number
}

export type RsvpStatus = 'going' | 'maybe' | 'not_going'

export interface GuildEvent {
  id: string
  guildId: string
  channelId: string | null
  title: string
  description: string | null
  startsAt: string
  endsAt: string | null
  createdBy: string
  createdAt: string
  rsvpCounts: RsvpCounts
  public: boolean
  // Issue #463 — the caller's own RSVP status, null if they haven't
  // responded. Never another member's; lets a control pre-select
  // correctly without a separate roster fetch.
  myRsvp: RsvpStatus | null
  // Issue #458 — false when the caller has `view` but not `view_details`
  // on this event: id/guildId/title/startsAt/endsAt/createdBy/createdAt/
  // public are real, but channelId/description/rsvpCounts/myRsvp are
  // placeholder values, not real data. Always true for create/update/RSVP
  // responses.
  detailsVisible: boolean
}
type GuildEventWire = components['schemas']['EventResponse']
function eventFromWire(w: GuildEventWire): GuildEvent {
  return {
    id: w.id,
    guildId: w.guild_id,
    channelId: w.channel_id ?? null,
    title: w.title,
    description: w.description ?? null,
    startsAt: w.starts_at,
    endsAt: w.ends_at ?? null,
    createdBy: w.created_by,
    createdAt: w.created_at,
    rsvpCounts: { going: w.rsvp_counts.going, maybe: w.rsvp_counts.maybe, notGoing: w.rsvp_counts.not_going },
    public: w.public ?? false,
    myRsvp: (w.my_rsvp as RsvpStatus | null | undefined) ?? null,
    detailsVisible: w.details_visible ?? true,
  }
}

export interface Rsvp {
  eventId: string
  identityId: string
  status: RsvpStatus
  respondedAt: string
}
type RsvpWire = components['schemas']['RsvpResponse']

export interface RsvpRosterEntry {
  identityId: string
  status: RsvpStatus
  respondedAt: string
}
type RsvpRosterEntryWire = components['schemas']['RsvpRosterEntry']

// Issue #206: aggregated count of guild members holding an active
// IntegratorBinding, per integrator, computed on read.
export interface GameBreakdownEntry {
  integratorId: string
  integratorSlug: string
  integratorName: string
  memberCount: number
}

export interface GameBreakdown {
  guildId: string
  totalMembers: number
  breakdown: GameBreakdownEntry[]
}
type GameBreakdownWire = components['schemas']['GameBreakdownResponse']
function gameBreakdownFromWire(w: GameBreakdownWire): GameBreakdown {
  return {
    guildId: w.guild_id,
    totalMembers: w.total_members,
    breakdown: w.breakdown.map((e) => ({
      integratorId: e.integrator_id,
      integratorSlug: e.integrator_slug,
      integratorName: e.integrator_name,
      memberCount: e.member_count,
    })),
  }
}

// Issue #253/#464 — same shape as GuildMessage plus archivedAt, over the
// long-window archive tier the live table's retention cap prunes into
// instead of hard-deleting.
export interface ArchivedMessage {
  id: string
  channelId: string
  author: string
  body: string
  sentAt: string
  archivedAt: string
}
type ArchivedMessageWire = components['schemas']['ArchivedMessageResponse']
function archivedMessageFromWire(w: ArchivedMessageWire): ArchivedMessage {
  return { id: w.id, channelId: w.channel_id, author: w.author, body: w.body, sentAt: w.sent_at, archivedAt: w.archived_at }
}

/** A partial update to a guild's own metadata — `undefined` leaves that
 * field untouched, matching `PATCH /guilds/{id}`'s own convention. */
export interface GuildUpdate {
  name?: string
  tag?: string
  description?: string
  motd?: string
  banner?: string
  icon?: string
  // Two-state, not three: omitted leaves it untouched, any array (incl.
  // []) always fully replaces the stored list.
  links?: GuildLink[]
  recruiting?: boolean
  public?: boolean
  // "invite_only" or "open" — see Guild.joinPolicy's own doc comment.
  joinPolicy?: string
  gameBreakdownPublic?: boolean
  // "public" | "guild_members" | "private".
  rosterVisibility?: string
}

/** A partial update to a channel — `undefined` leaves that field
 * untouched, except `name`, which is always resent. */
export interface ChannelUpdate {
  name: string
  announcementOnly?: boolean
  topic?: string
  public?: boolean
}

/** Fields for creating or fully replacing a guild event — full
 * replacement on update, matching `PATCH /guilds/{id}/events/{eid}`. */
export interface EventFields {
  channelId?: string
  title: string
  description?: string
  startsAt: string
  endsAt?: string
  public: boolean
}

declare module './core.js' {
  interface AccountSession {
    /** `POST /guilds` — creates a new guild, the caller as owner. Not
     * signature-required. */
    createGuild(name: string, tag: string, description: string): Promise<Guild>
    getGuild(guildId: string): Promise<Guild>
    /** `GET /guilds/discover{queryString}` — `queryString` is passed
     * through as-is (including its leading `?`). */
    discoverGuilds(queryString: string): Promise<DiscoverGuildsPage>
    updateGuild(guildId: string, update: GuildUpdate): Promise<Guild>
    listRoles(guildId: string): Promise<Role[]>
    /** Always signs (`guild.role.create`, `[guildId, name, permissions
     * comma-joined]`). `badge` omitted entirely means the server's own
     * default badge, not "leave unset" (there's no existing role to leave
     * anything on). */
    createRole(
      guildId: string,
      name: string,
      permissions: string[],
      description: string,
      badge?: RoleBadgeUpdate,
    ): Promise<Role>
    /** Always signs (`guild.role.update`, `[guildId, nameIndex]`). `badge`
     * omitted leaves the existing badge untouched. */
    updateRole(
      guildId: string,
      nameIndex: number,
      name?: string,
      permissions?: string[],
      description?: string,
      badge?: RoleBadgeUpdate,
    ): Promise<Role>
    /** Always signs (`guild.role.delete`, `[guildId, nameIndex]`). */
    deleteRole(guildId: string, nameIndex: number): Promise<void>
    listPermissionOverrides(guildId: string, resourceKind: string, resourceId: string): Promise<PermissionOverride[]>
    /** Always signs (`guild.permission_override.set`, `[guildId,
     * roleIndex, resourceKind, resourceId, permission, allow]`). */
    setPermissionOverride(
      guildId: string,
      roleIndex: number,
      resourceKind: string,
      resourceId: string,
      permission: string,
      allow: boolean,
    ): Promise<PermissionOverride>
    /** Always signs (`guild.permission_override.delete`, `[guildId,
     * overrideId]`). */
    deletePermissionOverride(guildId: string, overrideId: string): Promise<void>
    /** Owner-only, always signs (`guild.transfer_ownership`, `[guildId,
     * currentOwner, to]`). */
    transferOwnership(guildId: string, to: string): Promise<Guild>
    /** Not signature-required. */
    associateIntegrator(guildId: string, integratorId: string): Promise<Guild>
    listMembers(guildId: string): Promise<GuildMember[]>
    /** Always signs (`guild.member_role.update`, `[guildId, identityId,
     * roleIndex]`), whether or not this change actually escalates. */
    updateMemberRole(guildId: string, identityId: string, roleIndex: number): Promise<GuildMember>
    /** Kick, not a role change; reversible via re-invite. Not
     * signature-required. */
    removeMember(guildId: string, identityId: string): Promise<void>
    myGuilds(): Promise<MyGuildMembership[]>
    myGuildInvites(): Promise<MyGuildInvite[]>
    createGuildInvite(guildId: string, to: string): Promise<GuildInvite>
    acceptGuildInvite(guildId: string, inviteId: string): Promise<GuildMember>
    declineGuildInvite(guildId: string, inviteId: string): Promise<void>
    joinGuild(guildId: string): Promise<GuildMember>
    leaveGuild(guildId: string): Promise<void>
    createJoinRequest(guildId: string, message?: string): Promise<GuildJoinRequest>
    listJoinRequests(guildId: string): Promise<GuildJoinRequest[]>
    myJoinRequest(guildId: string): Promise<GuildJoinRequest | null>
    approveJoinRequest(guildId: string, requestId: string): Promise<GuildMember>
    rejectJoinRequest(guildId: string, requestId: string): Promise<void>
    withdrawJoinRequest(guildId: string, requestId: string): Promise<void>
    favoriteGames(guildId: string): Promise<FavoriteGames>
    /** `manage_guild`-gated, full ordered replacement. Not
     * signature-required. */
    setFavoriteGames(guildId: string, integratorIds: string[]): Promise<FavoriteGames>
    listChannels(guildId: string): Promise<GuildChannel[]>
    /** `manage_channels`-gated. Not signature-required (structural but
     * reversible). */
    createChannel(guildId: string, name: string): Promise<GuildChannel>
    updateChannel(guildId: string, channelId: string, update: ChannelUpdate): Promise<GuildChannel>
    archiveChannel(guildId: string, channelId: string): Promise<GuildChannel>
    channelMessages(guildId: string, channelId: string, before?: string, limit?: number): Promise<GuildMessage[]>
    /** Not signature-required (chat). */
    sendMessage(guildId: string, channelId: string, body: string): Promise<GuildMessage>
    /** Moderation delete, `manage_channels`-gated. Not
     * signature-required. */
    deleteMessage(guildId: string, channelId: string, messageId: string): Promise<void>
    listEvents(guildId: string, from?: string, to?: string): Promise<GuildEvent[]>
    /** Not signature-required (reversible scheduling state). */
    createEvent(guildId: string, fields: EventFields): Promise<GuildEvent>
    /** Full replacement, not partial. */
    updateEvent(guildId: string, eventId: string, fields: EventFields): Promise<GuildEvent>
    deleteEvent(guildId: string, eventId: string): Promise<void>
    rsvpToEvent(guildId: string, eventId: string, status: RsvpStatus): Promise<Rsvp>
    eventRsvps(guildId: string, eventId: string): Promise<RsvpRosterEntry[]>
    /** `GET /guilds/{id}/integrator-breakdown` — gated server-side to a
     * `manage_guild` holder (always) or anyone when the guild has set
     * `gameBreakdownPublic`. A 403 here is expected, not a bug. */
    getGameBreakdown(guildId: string): Promise<GameBreakdown>
    /** `GET /guilds/{id}/channels/{channelId}/messages/archive` — same
     * before/limit cursor shape as `channelMessages`, over the long-window
     * retention-pruned tier. Requires current guild membership. */
    getMessageArchive(
      guildId: string,
      channelId: string,
      before?: string,
      limit?: number,
    ): Promise<ArchivedMessage[]>
  }
}

AccountSession.prototype.createGuild = async function (
  this: AccountSession,
  name: string,
  tag: string,
  description: string,
): Promise<Guild> {
  const w = await this.post<GuildWire>('/guilds', { name, tag, description })
  return guildFromWire(w)
}

AccountSession.prototype.getGuild = async function (this: AccountSession, guildId: string): Promise<Guild> {
  return guildFromWire(await this.get<GuildWire>(`/guilds/${guildId}`))
}

AccountSession.prototype.discoverGuilds = async function (
  this: AccountSession,
  queryString: string,
): Promise<DiscoverGuildsPage> {
  const w = await this.get<DiscoverGuildsPageWire>(`/guilds/discover${queryString}`)
  if (!Array.isArray(w?.guilds)) return w as unknown as DiscoverGuildsPage
  return {
    guilds: w.guilds.map((g) => ({
      id: g.id,
      name: g.name,
      tag: g.tag,
      description: g.description,
      recruiting: g.recruiting,
      memberCount: g.member_count,
      createdAt: g.created_at,
      banner: g.banner ?? null,
      icon: g.icon ?? null,
    })),
    nextCursor: w.next_cursor ?? null,
  }
}

AccountSession.prototype.updateGuild = async function (
  this: AccountSession,
  guildId: string,
  update: GuildUpdate,
): Promise<Guild> {
  const w = await this.patch<GuildWire>(`/guilds/${guildId}`, {
    name: update.name,
    tag: update.tag,
    description: update.description,
    motd: update.motd,
    banner: update.banner,
    icon: update.icon,
    links: update.links,
    recruiting: update.recruiting,
    public: update.public,
    join_policy: update.joinPolicy,
    game_breakdown_public: update.gameBreakdownPublic,
    roster_visibility: update.rosterVisibility,
  })
  return guildFromWire(w)
}

AccountSession.prototype.listRoles = async function (this: AccountSession, guildId: string): Promise<Role[]> {
  const w = await this.get<RoleWire[]>(`/guilds/${guildId}/roles`)
  if (!Array.isArray(w)) return w as unknown as Role[]
  return w.map(roleFromWire)
}

AccountSession.prototype.createRole = async function (
  this: AccountSession,
  guildId: string,
  name: string,
  permissions: string[],
  description: string,
  badge?: RoleBadgeUpdate,
): Promise<Role> {
  const signature = this.sign('guild.role.create', [guildId, name, permissions.join(',')])
  const w = await this.post<RoleWire>(`/guilds/${guildId}/roles`, {
    name,
    permissions,
    description,
    badge,
    ...signature,
  })
  return roleFromWire(w)
}

AccountSession.prototype.updateRole = async function (
  this: AccountSession,
  guildId: string,
  nameIndex: number,
  name?: string,
  permissions?: string[],
  description?: string,
  badge?: RoleBadgeUpdate,
): Promise<Role> {
  const signature = this.sign('guild.role.update', [guildId, String(nameIndex)])
  const w = await this.patch<RoleWire>(`/guilds/${guildId}/roles/${nameIndex}`, {
    name,
    permissions,
    description,
    badge,
    ...signature,
  })
  return roleFromWire(w)
}

AccountSession.prototype.deleteRole = async function (
  this: AccountSession,
  guildId: string,
  nameIndex: number,
): Promise<void> {
  const signature = this.sign('guild.role.delete', [guildId, String(nameIndex)])
  await this.deleteWithBody(`/guilds/${guildId}/roles/${nameIndex}`, signature)
}

AccountSession.prototype.listPermissionOverrides = async function (
  this: AccountSession,
  guildId: string,
  resourceKind: string,
  resourceId: string,
): Promise<PermissionOverride[]> {
  const w = await this.getQuery<PermissionOverrideWire[]>(`/guilds/${guildId}/permission-overrides`, {
    resource_kind: resourceKind,
    resource_id: resourceId,
  })
  if (!Array.isArray(w)) return w as unknown as PermissionOverride[]
  return w.map(overrideFromWire)
}

AccountSession.prototype.setPermissionOverride = async function (
  this: AccountSession,
  guildId: string,
  roleIndex: number,
  resourceKind: string,
  resourceId: string,
  permission: string,
  allow: boolean,
): Promise<PermissionOverride> {
  const signature = this.sign('guild.permission_override.set', [
    guildId,
    String(roleIndex),
    resourceKind,
    resourceId,
    permission,
    String(allow),
  ])
  const w = await this.put<PermissionOverrideWire>(`/guilds/${guildId}/permission-overrides`, {
    role_index: roleIndex,
    resource_kind: resourceKind,
    resource_id: resourceId,
    permission,
    allow,
    ...signature,
  })
  return overrideFromWire(w)
}

AccountSession.prototype.deletePermissionOverride = async function (
  this: AccountSession,
  guildId: string,
  overrideId: string,
): Promise<void> {
  const signature = this.sign('guild.permission_override.delete', [guildId, overrideId])
  await this.deleteWithBody(`/guilds/${guildId}/permission-overrides/${overrideId}`, signature)
}

AccountSession.prototype.transferOwnership = async function (
  this: AccountSession,
  guildId: string,
  to: string,
): Promise<Guild> {
  const signature = this.sign('guild.transfer_ownership', [guildId, this.identity().id, to])
  const w = await this.post<GuildWire>(`/guilds/${guildId}/transfer-ownership`, { to, ...signature })
  return guildFromWire(w)
}

AccountSession.prototype.associateIntegrator = async function (
  this: AccountSession,
  guildId: string,
  integratorId: string,
): Promise<Guild> {
  return guildFromWire(await this.postEmpty<GuildWire>(`/guilds/${guildId}/integrations/${integratorId}`))
}

AccountSession.prototype.listMembers = async function (
  this: AccountSession,
  guildId: string,
): Promise<GuildMember[]> {
  const w = await this.get<GuildMemberWire[]>(`/guilds/${guildId}/members`)
  if (!Array.isArray(w)) return w as unknown as GuildMember[]
  return w.map(memberFromWire)
}

AccountSession.prototype.updateMemberRole = async function (
  this: AccountSession,
  guildId: string,
  identityId: string,
  roleIndex: number,
): Promise<GuildMember> {
  const signature = this.sign('guild.member_role.update', [guildId, identityId, String(roleIndex)])
  const w = await this.patch<GuildMemberWire>(`/guilds/${guildId}/members/${identityId}`, {
    role_index: roleIndex,
    ...signature,
  })
  return memberFromWire(w)
}

AccountSession.prototype.removeMember = async function (
  this: AccountSession,
  guildId: string,
  identityId: string,
): Promise<void> {
  await this.del(`/guilds/${guildId}/members/${identityId}`)
}

AccountSession.prototype.myGuilds = async function (this: AccountSession): Promise<MyGuildMembership[]> {
  const w = await this.get<components['schemas']['MyGuildMembershipResponse'][]>('/me/guilds')
  if (!Array.isArray(w)) return w as unknown as MyGuildMembership[]
  return w.map((m) => ({ guildId: m.guild_id, roleIndex: m.role_index, joinedAt: m.joined_at }))
}

AccountSession.prototype.myGuildInvites = async function (this: AccountSession): Promise<MyGuildInvite[]> {
  const w = await this.get<MyGuildInviteWire[]>('/me/guild-invites')
  if (!Array.isArray(w)) return w as unknown as MyGuildInvite[]
  return w.map((i) => ({ id: i.id, guildId: i.guild_id, guildName: i.guild_name, from: i.from, createdAt: i.created_at }))
}

AccountSession.prototype.createGuildInvite = async function (
  this: AccountSession,
  guildId: string,
  to: string,
): Promise<GuildInvite> {
  const w = await this.post<GuildInviteWire>(`/guilds/${guildId}/invites`, { to })
  return { id: w.id, guildId: w.guild_id, to: w.to, from: w.from, createdAt: w.created_at }
}

AccountSession.prototype.acceptGuildInvite = async function (
  this: AccountSession,
  guildId: string,
  inviteId: string,
): Promise<GuildMember> {
  return memberFromWire(await this.postEmpty<GuildMemberWire>(`/guilds/${guildId}/invites/${inviteId}/accept`))
}

AccountSession.prototype.declineGuildInvite = async function (
  this: AccountSession,
  guildId: string,
  inviteId: string,
): Promise<void> {
  await this.postEmptyNoResponse(`/guilds/${guildId}/invites/${inviteId}/decline`)
}

AccountSession.prototype.joinGuild = async function (this: AccountSession, guildId: string): Promise<GuildMember> {
  return memberFromWire(await this.postEmpty<GuildMemberWire>(`/guilds/${guildId}/join`))
}

AccountSession.prototype.leaveGuild = async function (this: AccountSession, guildId: string): Promise<void> {
  await this.postEmptyNoResponse(`/guilds/${guildId}/leave`)
}

AccountSession.prototype.createJoinRequest = async function (
  this: AccountSession,
  guildId: string,
  message?: string,
): Promise<GuildJoinRequest> {
  const w = await this.post<GuildJoinRequestWire>(`/guilds/${guildId}/join-requests`, { message: message ?? null })
  return joinRequestFromWire(w)
}

AccountSession.prototype.listJoinRequests = async function (
  this: AccountSession,
  guildId: string,
): Promise<GuildJoinRequest[]> {
  const w = await this.get<GuildJoinRequestWire[]>(`/guilds/${guildId}/join-requests`)
  if (!Array.isArray(w)) return w as unknown as GuildJoinRequest[]
  return w.map(joinRequestFromWire)
}

AccountSession.prototype.myJoinRequest = async function (
  this: AccountSession,
  guildId: string,
): Promise<GuildJoinRequest | null> {
  const w = await this.get<GuildJoinRequestWire | null>(`/guilds/${guildId}/join-requests/mine`)
  return w ? joinRequestFromWire(w) : null
}

AccountSession.prototype.approveJoinRequest = async function (
  this: AccountSession,
  guildId: string,
  requestId: string,
): Promise<GuildMember> {
  return memberFromWire(
    await this.postEmpty<GuildMemberWire>(`/guilds/${guildId}/join-requests/${requestId}/approve`),
  )
}

AccountSession.prototype.rejectJoinRequest = async function (
  this: AccountSession,
  guildId: string,
  requestId: string,
): Promise<void> {
  await this.postEmptyNoResponse(`/guilds/${guildId}/join-requests/${requestId}/reject`)
}

AccountSession.prototype.withdrawJoinRequest = async function (
  this: AccountSession,
  guildId: string,
  requestId: string,
): Promise<void> {
  await this.del(`/guilds/${guildId}/join-requests/${requestId}`)
}

AccountSession.prototype.favoriteGames = async function (
  this: AccountSession,
  guildId: string,
): Promise<FavoriteGames> {
  const w = await this.get<FavoriteGamesWire>(`/guilds/${guildId}/favorite-integrators`)
  return { guildId: w.guild_id, favorites: w.favorites.map(favoriteGameEntryFromWire) }
}

AccountSession.prototype.setFavoriteGames = async function (
  this: AccountSession,
  guildId: string,
  integratorIds: string[],
): Promise<FavoriteGames> {
  const w = await this.put<FavoriteGamesWire>(`/guilds/${guildId}/favorite-integrators`, {
    integrator_ids: integratorIds,
  })
  return { guildId: w.guild_id, favorites: w.favorites.map(favoriteGameEntryFromWire) }
}

AccountSession.prototype.listChannels = async function (
  this: AccountSession,
  guildId: string,
): Promise<GuildChannel[]> {
  const w = await this.get<GuildChannelWire[]>(`/guilds/${guildId}/channels`)
  if (!Array.isArray(w)) return w as unknown as GuildChannel[]
  return w.map(channelFromWire)
}

AccountSession.prototype.createChannel = async function (
  this: AccountSession,
  guildId: string,
  name: string,
): Promise<GuildChannel> {
  return channelFromWire(await this.post<GuildChannelWire>(`/guilds/${guildId}/channels`, { name }))
}

AccountSession.prototype.updateChannel = async function (
  this: AccountSession,
  guildId: string,
  channelId: string,
  update: ChannelUpdate,
): Promise<GuildChannel> {
  const w = await this.patch<GuildChannelWire>(`/guilds/${guildId}/channels/${channelId}`, {
    name: update.name,
    announcement_only: update.announcementOnly,
    topic: update.topic,
    public: update.public,
  })
  return channelFromWire(w)
}

AccountSession.prototype.archiveChannel = async function (
  this: AccountSession,
  guildId: string,
  channelId: string,
): Promise<GuildChannel> {
  return channelFromWire(await this.postEmpty<GuildChannelWire>(`/guilds/${guildId}/channels/${channelId}/archive`))
}

AccountSession.prototype.channelMessages = async function (
  this: AccountSession,
  guildId: string,
  channelId: string,
  before?: string,
  limit?: number,
): Promise<GuildMessage[]> {
  const query: Record<string, string> = {}
  if (before) query.before = before
  if (limit !== undefined) query.limit = String(limit)
  const w = await this.getQuery<GuildMessageWire[]>(`/guilds/${guildId}/channels/${channelId}/messages`, query)
  if (!Array.isArray(w)) return w as unknown as GuildMessage[]
  return w.map(guildMessageFromWire)
}

AccountSession.prototype.sendMessage = async function (
  this: AccountSession,
  guildId: string,
  channelId: string,
  body: string,
): Promise<GuildMessage> {
  return guildMessageFromWire(
    await this.post<GuildMessageWire>(`/guilds/${guildId}/channels/${channelId}/messages`, { body }),
  )
}

AccountSession.prototype.deleteMessage = async function (
  this: AccountSession,
  guildId: string,
  channelId: string,
  messageId: string,
): Promise<void> {
  await this.del(`/guilds/${guildId}/channels/${channelId}/messages/${messageId}`)
}

AccountSession.prototype.listEvents = async function (
  this: AccountSession,
  guildId: string,
  from?: string,
  to?: string,
): Promise<GuildEvent[]> {
  const query: Record<string, string> = {}
  if (from) query.from = from
  if (to) query.to = to
  const w = await this.getQuery<GuildEventWire[]>(`/guilds/${guildId}/events`, query)
  if (!Array.isArray(w)) return w as unknown as GuildEvent[]
  return w.map(eventFromWire)
}

function eventRequestBody(fields: EventFields) {
  return {
    channel_id: fields.channelId ?? null,
    title: fields.title,
    description: fields.description ?? null,
    starts_at: fields.startsAt,
    ends_at: fields.endsAt ?? null,
    public: fields.public,
  }
}

AccountSession.prototype.createEvent = async function (
  this: AccountSession,
  guildId: string,
  fields: EventFields,
): Promise<GuildEvent> {
  return eventFromWire(await this.post<GuildEventWire>(`/guilds/${guildId}/events`, eventRequestBody(fields)))
}

AccountSession.prototype.updateEvent = async function (
  this: AccountSession,
  guildId: string,
  eventId: string,
  fields: EventFields,
): Promise<GuildEvent> {
  return eventFromWire(
    await this.patch<GuildEventWire>(`/guilds/${guildId}/events/${eventId}`, eventRequestBody(fields)),
  )
}

AccountSession.prototype.deleteEvent = async function (
  this: AccountSession,
  guildId: string,
  eventId: string,
): Promise<void> {
  await this.del(`/guilds/${guildId}/events/${eventId}`)
}

AccountSession.prototype.rsvpToEvent = async function (
  this: AccountSession,
  guildId: string,
  eventId: string,
  status: RsvpStatus,
): Promise<Rsvp> {
  const w = await this.put<RsvpWire>(`/guilds/${guildId}/events/${eventId}/rsvp`, { status })
  return { eventId: w.event_id, identityId: w.identity_id, status: w.status as RsvpStatus, respondedAt: w.responded_at }
}

AccountSession.prototype.eventRsvps = async function (
  this: AccountSession,
  guildId: string,
  eventId: string,
): Promise<RsvpRosterEntry[]> {
  const w = await this.get<RsvpRosterEntryWire[]>(`/guilds/${guildId}/events/${eventId}/rsvps`)
  if (!Array.isArray(w)) return w as unknown as RsvpRosterEntry[]
  return w.map((r) => ({ identityId: r.identity_id, status: r.status as RsvpStatus, respondedAt: r.responded_at }))
}

AccountSession.prototype.getGameBreakdown = async function (
  this: AccountSession,
  guildId: string,
): Promise<GameBreakdown> {
  return gameBreakdownFromWire(await this.get<GameBreakdownWire>(`/guilds/${guildId}/integrator-breakdown`))
}

AccountSession.prototype.getMessageArchive = async function (
  this: AccountSession,
  guildId: string,
  channelId: string,
  before?: string,
  limit?: number,
): Promise<ArchivedMessage[]> {
  const query: Record<string, string> = {}
  if (before) query.before = before
  if (limit !== undefined) query.limit = String(limit)
  const w = await this.getQuery<ArchivedMessageWire[]>(
    `/guilds/${guildId}/channels/${channelId}/messages/archive`,
    query,
  )
  if (!Array.isArray(w)) return w as unknown as ArchivedMessage[]
  return w.map(archivedMessageFromWire)
}
