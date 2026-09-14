// Orchestrates guild rosters + presence + display names for issue #24:
// fetches all three separately and merges them client-side, since
// GET /guilds/{id}/members embeds neither presence nor a display name
// server-side (see crates/server/src/guilds.rs) — the same gap
// apps/hub/src/api/friends.ts's listFriendsWithPresence already documents
// and merges around for friends. Display names are resolved via
// GET /identities/profiles (issue #161); mirrors that module's shape
// closely.
import * as api from './client'
import type {
  DiscoverGuildsParams,
  DiscoverGuildSummary,
  FavoriteGameEntry,
  GameBreakdownEntry,
  GuildMemberResponse,
  GuildResponse,
  PresenceResponse,
  PresenceStatus,
  PublicProfileResponse,
  RoleResponse,
} from './types'

export interface GuildMember {
  identityId: string
  // Undefined only if #161's batch lookup has no profile for this id
  // (shouldn't happen for a real member, but the UI still falls back to a
  // shortened id rather than assuming).
  displayName?: string
  roleIndex: number
  status: PresenceStatus
  // Mirrors PresenceResponse.playing exactly: an integrator id, or null/absent.
  // Always null in practice today (types.ts's own note — no real integrator
  // publishes presence yet), carried through here so the "members
  // currently playing" summary (#57) is correct once an integrator does, rather
  // than needing a second wiring pass later. Optional (not just
  // nullable) so existing fixtures/tests built before #57 don't all need
  // updating — absent is treated identically to null everywhere it's read.
  playing?: string | null
  joinedAt: string
}

// Pure merge logic, testable without any network call — mirrors
// apps/hub/src/api/friends.ts's mergeFriend. A missing presence entry
// defaults to Offline/not-playing rather than guessing at "last seen"
// (#78, presence honesty). Takes the presence-by-status map (not the full
// PresenceResponse) for status, plus a parallel presence-by-playing map,
// so existing callers/tests that only care about status keep working
// unchanged.
export function mergeGuildMember(
  member: GuildMemberResponse,
  presenceByStatus: Map<string, PresenceStatus>,
  displayNameById: Map<string, string> = new Map(),
  presenceByPlaying: Map<string, string | null> = new Map(),
): GuildMember {
  return {
    identityId: member.identity_id,
    displayName: displayNameById.get(member.identity_id),
    roleIndex: member.role_index,
    status: presenceByStatus.get(member.identity_id) ?? 'Offline',
    playing: presenceByPlaying.get(member.identity_id) ?? null,
    joinedAt: member.joined_at,
  }
}

export async function listMembersWithPresence(token: string, guildId: string): Promise<GuildMember[]> {
  const members = await api.listMembers(token, guildId)
  if (members.length === 0) {
    return []
  }

  const ids = members.map((m) => m.identity_id)
  const [presences, profiles] = await Promise.all([
    api.getPresence(token, ids),
    api.getProfiles(token, ids),
  ])
  const presenceByStatus = new Map<string, PresenceStatus>(
    presences.map((p: PresenceResponse) => [p.identity_id, p.status]),
  )
  const presenceByPlaying = new Map<string, string | null>(
    presences.map((p: PresenceResponse) => [p.identity_id, p.active_in]),
  )
  const displayNameById = new Map<string, string>(
    profiles.map((p: PublicProfileResponse) => [p.identity_id, p.display_name]),
  )

  return members.map((m) => mergeGuildMember(m, presenceByStatus, displayNameById, presenceByPlaying))
}

export interface RoleGroup {
  roleIndex: number
  roleName: string
  members: GuildMember[]
}

// Groups a roster by role index, ordered lowest (owner, 0) first — pure,
// unit-testable independent of any fetch. A role with no current members
// is omitted rather than shown empty (the ticket's "grouped by role", not
// "every defined role").
export function groupMembersByRole(members: GuildMember[], roles: RoleResponse[]): RoleGroup[] {
  const roleNameByIndex = new Map(roles.map((r) => [r.name_index, r.name]))
  const groups = new Map<number, GuildMember[]>()
  for (const member of members) {
    const bucket = groups.get(member.roleIndex)
    if (bucket) {
      bucket.push(member)
    } else {
      groups.set(member.roleIndex, [member])
    }
  }

  return Array.from(groups.entries())
    .sort(([a], [b]) => a - b)
    .map(([roleIndex, groupMembers]) => ({
      roleIndex,
      roleName: roleNameByIndex.get(roleIndex) ?? `Role ${roleIndex}`,
      members: groupMembers,
    }))
}

// Plain status line describing the caller's relationship to a guild,
// shown in the "Membership" card regardless of which action buttons (if
// any) also apply — a guild owner of an invite-only guild otherwise sees
// neither a Join nor a Leave button, and an otherwise-empty card reads as
// broken rather than simply "nothing to do here". Pure, unit-testable.
export function membershipStatusText(isOwner: boolean, isMember: boolean): string {
  if (isOwner) return 'You are the owner of this guild.'
  if (isMember) return 'You are a member.'
  return 'You are not a member of this guild.'
}

// Splits a roster into online vs. offline, online first — the same
// online-before-offline precedent apps/hub/src/composables/useFriendsPresence.ts
// already establishes for friends (an 'Away' member counts as online, only
// 'Offline' is offline). Pure, unit-testable independent of any fetch.
export function sortMembersByPresence(members: GuildMember[]): GuildMember[] {
  const online = members.filter((m) => m.status !== 'Offline')
  const offline = members.filter((m) => m.status === 'Offline')
  return [...online, ...offline]
}

// Case-insensitive, partial match against a guild's name or tag — for the
// "my guilds" list on apps/hub/src/views/Guilds.vue. Pure, unit-testable
// independent of any fetch.
export function filterGuildsByNameOrTag<T extends Pick<GuildResponse, 'name' | 'tag'>>(
  guilds: T[],
  query: string,
): T[] {
  const needle = query.trim().toLowerCase()
  if (!needle) {
    return guilds
  }
  return guilds.filter(
    (g) => g.name.toLowerCase().includes(needle) || g.tag.toLowerCase().includes(needle),
  )
}

// Case-insensitive, partial match against identity id — the only field
// there's anything to search on until #161 (batch identity lookup)
// resolves display names. Pure, unit-testable independent of any fetch.
// Matches against the resolved display name when available (#161), and
// always against the identity id too — a caller who still only has an id
// on hand (e.g. from an invite) can still find the row.
export function filterMembersByIdentityId(members: GuildMember[], query: string): GuildMember[] {
  const needle = query.trim().toLowerCase()
  if (!needle) {
    return members
  }
  return members.filter(
    (m) => m.identityId.toLowerCase().includes(needle) || (m.displayName?.toLowerCase().includes(needle) ?? false),
  )
}

export type MemberSortOrder = 'role' | 'name'

// "by name" sorts by the resolved display name (#161) when available,
// falling back to the identity id for any member it isn't (shouldn't
// normally happen, but stays honest rather than assuming). "by role" is a
// no-op here; role ordering is applied by groupMembersByRole itself, so
// this exists to give the UI a single sort-order switch that covers both
// cases.
export function sortMembers(members: GuildMember[], order: MemberSortOrder): GuildMember[] {
  if (order === 'name') {
    return [...members].sort((a, b) => (a.displayName ?? a.identityId).localeCompare(b.displayName ?? b.identityId))
  }
  return members
}

// Mirrors crates/server/src/guilds.rs::has_guild_permission exactly: the
// guild owner can always act, structurally, regardless of their role's
// permission list — never derived from role_index/permissions alone. Used
// to decide which management controls to render; the server re-checks
// independently and is the real authority (this is UI gating only, per the
// ticket's invariant — a rejected 403 must not crash the page).
export function hasGuildPermission(
  guild: Pick<GuildResponse, 'owner'>,
  actorIdentityId: string,
  actorPermissions: string[],
  permission: string,
): boolean {
  if (actorIdentityId === guild.owner) {
    return true
  }
  return actorPermissions.includes(permission)
}

// Fixed starter role indices (crates/server/src/guilds.rs's
// OWNER_ROLE_INDEX/OFFICER_ROLE_INDEX/MEMBER_ROLE_INDEX) — only the plain
// "member" tier matters client-side, for can_remove_member's own
// distinction below.
const MEMBER_ROLE_INDEX = 2

// Mirrors crates/server/src/guilds.rs::can_remove_member exactly: the
// owner can never be kicked regardless of permissions; a plain member can
// be kicked with just `manage_members`; removing anyone in an elevated
// role additionally requires `manage_roles` (or being the owner). UI
// gating only — the server re-checks and is the real authority.
export function canKickMember(
  guild: Pick<GuildResponse, 'owner'>,
  actorIdentityId: string,
  actorPermissions: string[],
  target: GuildMember,
): boolean {
  if (target.identityId === guild.owner) {
    return false
  }
  if (!hasGuildPermission(guild, actorIdentityId, actorPermissions, 'manage_members')) {
    return false
  }
  if (target.roleIndex === MEMBER_ROLE_INDEX) {
    return true
  }
  return (
    actorIdentityId === guild.owner ||
    hasGuildPermission(guild, actorIdentityId, actorPermissions, 'manage_roles')
  )
}

// Mirrors crates/server/src/guilds.rs::update_member_role: requires
// `manage_roles`, and the owner's role can never be reassigned this way
// (ownership only moves via transfer-ownership).
export function canChangeMemberRole(
  guild: Pick<GuildResponse, 'owner'>,
  actorIdentityId: string,
  actorPermissions: string[],
  target: GuildMember,
): boolean {
  if (target.identityId === guild.owner) {
    return false
  }
  return hasGuildPermission(guild, actorIdentityId, actorPermissions, 'manage_roles')
}

// "Members currently playing", grouped by integrator id, counts descending
// (#57). Pure, unit-testable independent of any fetch. Only members with a
// non-null `playing` count toward any group — offline/idle/no-integrator members
// are simply absent from the result, not a "null" bucket, since there's
// nothing true to say about them per-integrator. This is realtime presence, not
// a durable guild stat: an integrator is never "the guild's integrator" (#74) — a
// member merely happens to be playing it right now. `playing` is always
// null in every guild today (no integrator has a live presence-publish binding
// yet, see api/types.ts's own note on PresenceResponse.active_in), so this
// resolves to an empty list in practice until that changes; the function
// itself doesn't assume that and works the same either way.
export interface PlayingGroup {
  integratorId: string
  count: number
}

export function groupMembersPlayingByIntegrator(members: GuildMember[]): PlayingGroup[] {
  const counts = new Map<string, number>()
  for (const member of members) {
    if (!member.playing) continue
    counts.set(member.playing, (counts.get(member.playing) ?? 0) + 1)
  }
  return Array.from(counts.entries())
    .map(([integratorId, count]) => ({ integratorId, count }))
    .sort((a, b) => b.count - a.count || a.integratorId.localeCompare(b.integratorId))
}

// Renders one PlayingGroup as "N members playing X" — the #74-safe
// phrasing the ticket requires verbatim: a member is described as playing
// an integrator, a guild is never described as belonging to one ("Integrator X's
// guild" is exactly what this must never read as).
export function formatPlayingSummary(group: PlayingGroup): string {
  const noun = group.count === 1 ? 'member' : 'members'
  return `${group.count} ${noun} playing ${group.integratorId}`
}

// Maps a role_index to AvalonRoleBadge's fixed-tier visual variant. Roles
// 0/1/2 are the fixed starter roles (owner/officer/member) every guild is
// seeded with (crates/server/src/guilds.rs::starter_roles); any custom
// role beyond that falls back to the neutral "member" styling.
export function roleVariantForIndex(roleIndex: number): 'owner' | 'officer' | 'member' {
  if (roleIndex === 0) return 'owner'
  if (roleIndex === 1) return 'officer'
  return 'member'
}

// The caller's own permission list, derived the same way
// crates/server/src/guilds.rs::actor_role_permissions does server-side:
// look up their role_index in the roster, then that role's permissions.
// Returns an empty list for a non-member (matching the server's own
// fallback) rather than throwing.
export function permissionsForMember(
  actorIdentityId: string,
  members: GuildMember[],
  roles: RoleResponse[],
): string[] {
  const self = members.find((m) => m.identityId === actorIdentityId)
  if (!self) {
    return []
  }
  return roles.find((r) => r.name_index === self.roleIndex)?.permissions ?? []
}

// Builds the `?q=&recruiting=&...` query string for GET /guilds/discover
// (issue #154) from a params object — pure and unit-testable independent
// of any fetch, same "logic stays out of client.ts" split every other
// function in this module follows. Omits a key entirely rather than
// sending an empty/undefined value, matching
// crates/server/src/guilds.rs::DiscoverGuildsQuery's "omitted means use the
// default" semantics (an empty string is not the same request as an
// omitted `q=`).
export function buildDiscoverQueryString(params: DiscoverGuildsParams): string {
  const search = new URLSearchParams()
  if (params.q && params.q.trim()) {
    search.set('q', params.q.trim())
  }
  if (params.recruiting !== undefined) {
    search.set('recruiting', String(params.recruiting))
  }
  if (params.tag && params.tag.trim()) {
    search.set('tag', params.tag.trim())
  }
  if (params.integrator) {
    search.set('game', params.integrator)
  }
  if (params.sort) {
    search.set('sort', params.sort)
  }
  if (params.limit !== undefined) {
    search.set('limit', String(params.limit))
  }
  if (params.cursor) {
    search.set('cursor', params.cursor)
  }
  const query = search.toString()
  return query ? `?${query}` : ''
}

// --- Integrator affinity breakdown (issue #206, implementing decision #160) -----

// Renders one GameBreakdownEntry against the guild's total membership as
// "N of M members play <integrator>" — the exact phrasing the ticket's design
// section illustrates ("14 of 22 members play Ashen Realms"). Pure and
// unit-testable independent of any fetch. `totalMembers` is the response's
// own `total_members`, not a sum of every entry's `member_count` — a
// member can be bound to zero, one, or several integrators, so those two numbers
// are never guaranteed equal.
export function formatGameBreakdownEntry(entry: GameBreakdownEntry, totalMembers: number): string {
  return `${entry.member_count} of ${totalMembers} members play ${entry.integrator_name}`
}

// True when the breakdown response itself has nothing to show — distinct
// from a 403 (not permitted to view it at all), which the caller handles
// separately as an error state, not an empty one.
export function hasNoGameBreakdownData(breakdown: GameBreakdownEntry[]): boolean {
  return breakdown.length === 0
}

// --- Favorite integrators pin list (issue #207, implementing decision #160) -----
// Mirrors crates/server/src/guilds.rs::MAX_GUILD_FAVORITE_GAMES exactly —
// the server is the real authority (a stale client constant here can only
// ever under- or over-disable the "Pin" button a request would then be
// rejected for anyway), kept in sync by hand same as every other numeric
// cap the hub duplicates from the server (see e.g. MAX_GUILD_LINKS not
// being wired here yet).
export const MAX_FAVORITE_GAMES = 5

// An integrator affinity breakdown entry is eligible to be pinned only while it
// isn't already pinned — the server independently re-derives "has real
// affinity" from the very breakdown this list is built from, so this
// helper's only job is de-duplication, not re-validating the affinity
// itself.
export function pinnableBreakdownEntries(
  breakdown: GameBreakdownEntry[],
  favorites: FavoriteGameEntry[],
): GameBreakdownEntry[] {
  const pinnedIds = new Set(favorites.map((f) => f.integrator_id))
  return breakdown.filter((entry) => !pinnedIds.has(entry.integrator_id))
}

export function canPinMoreFavorites(favorites: FavoriteGameEntry[]): boolean {
  return favorites.length < MAX_FAVORITE_GAMES
}

// Every mutation below returns the *next full ordered id list* to PUT —
// same "resend the whole list" convention crates/server/src/guilds.rs's
// `SetFavoriteGamesRequest` (and #153's `links` before it) already
// establishes; this module never does a partial/per-entry patch.
export function addFavoriteGameId(favorites: FavoriteGameEntry[], integratorId: string): string[] {
  return [...favorites.map((f) => f.integrator_id), integratorId]
}

export function removeFavoriteGameId(favorites: FavoriteGameEntry[], integratorId: string): string[] {
  return favorites.filter((f) => f.integrator_id !== integratorId).map((f) => f.integrator_id)
}

// Swaps `integratorId` with its neighbor one position earlier/later. A no-op
// (returns the unchanged order) if `integratorId` isn't found or is already at
// that end of the list — callers disable the button in that case, but this
// stays safe to call regardless.
export function reorderFavoriteGameIds(
  favorites: FavoriteGameEntry[],
  integratorId: string,
  direction: 'up' | 'down',
): string[] {
  const ids = favorites.map((f) => f.integrator_id)
  const index = ids.indexOf(integratorId)
  if (index === -1) return ids
  const swapWith = direction === 'up' ? index - 1 : index + 1
  if (swapWith < 0 || swapWith >= ids.length) return ids
  const next = [...ids]
  const temp = next[index]
  next[index] = next[swapWith]
  next[swapWith] = temp
  return next
}

// Renders one FavoriteGameEntry for display — flags staleness inline
// rather than hiding it, per #207's "surface, don't silently churn" design.
export function formatFavoriteGameEntry(entry: FavoriteGameEntry): string {
  return entry.stale ? `${entry.integrator_name} (no longer actively played)` : entry.integrator_name
}

// Issue #242: whether the Discover board should offer "Apply to join" for
// this guild — only when it's recruiting and the caller isn't already a
// member. `memberGuildIds` comes from useMyGuilds' own roster (the same
// source Guilds.vue's "My guilds" tab already fetches), not a second
// membership check. Pure, unit-testable independent of any fetch.
export function canApplyToJoinGuild(
  guild: Pick<DiscoverGuildSummary, 'id' | 'recruiting'>,
  memberGuildIds: string[],
): boolean {
  return guild.recruiting && !memberGuildIds.includes(guild.id)
}
