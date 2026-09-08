// Orchestrates guild rosters + presence for issue #24: fetches both
// separately and merges them client-side, since GET /guilds/{id}/members
// does not embed presence server-side (see crates/server/src/guilds.rs) —
// the same gap apps/hub/src/api/friends.ts's listFriendsWithPresence
// already documents and merges around for friends. Mirrors that module's
// shape closely.
import * as api from './client'
import type { GuildMemberResponse, GuildResponse, PresenceResponse, PresenceStatus, RoleResponse } from './types'

export interface GuildMember {
  identityId: string
  // Always undefined today — no endpoint resolves another identity's
  // display name yet, same gap apps/hub/src/api/friends.ts's Friend type
  // already documents.
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
): GuildMember {
  return {
    identityId: member.identity_id,
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

  const presences = await api.getPresence(
    token,
    members.map((m) => m.identity_id),
  )
  const presenceByStatus = new Map<string, PresenceStatus>(
    presences.map((p: PresenceResponse) => [p.identity_id, p.status]),
  )

  return members.map((m) => mergeGuildMember(m, presenceByStatus))
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
