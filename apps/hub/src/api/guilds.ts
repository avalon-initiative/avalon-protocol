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
  joinedAt: string
}

// Pure merge logic, testable without any network call — mirrors
// apps/hub/src/api/friends.ts's mergeFriend. A missing presence entry
// defaults to Offline rather than guessing at "last seen" (#78, presence
// honesty).
export function mergeGuildMember(
  member: GuildMemberResponse,
  presenceByStatus: Map<string, PresenceStatus>,
  displayNameById: Map<string, string> = new Map(),
): GuildMember {
  return {
    identityId: member.identity_id,
    displayName: displayNameById.get(member.identity_id),
    roleIndex: member.role_index,
    status: presenceByStatus.get(member.identity_id) ?? 'Offline',
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
  const displayNameById = new Map<string, string>(
    profiles.map((p: PublicProfileResponse) => [p.identity_id, p.display_name]),
  )

  return members.map((m) => mergeGuildMember(m, presenceByStatus, displayNameById))
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
