import { describe, expect, it } from 'vitest'
import {
  canChangeMemberRole,
  canKickMember,
  groupMembersByRole,
  hasGuildPermission,
  mergeGuildMember,
  permissionsForMember,
  roleVariantForIndex,
} from './guilds'
import type { GuildMember } from './guilds'
import type { GuildMemberResponse, RoleResponse } from './types'

const OWNER = 'owner-id'
const OFFICER = 'officer-id'
const MEMBER = 'member-id'

describe('mergeGuildMember', () => {
  it('defaults to Offline when the presence map has no entry', () => {
    const member: GuildMemberResponse = {
      guild_id: 'g1',
      identity_id: MEMBER,
      role_index: 2,
      joined_at: 't',
    }
    const merged = mergeGuildMember(member, new Map())
    expect(merged.status).toBe('Offline')
  })

  it('uses the presence map entry when present', () => {
    const member: GuildMemberResponse = {
      guild_id: 'g1',
      identity_id: MEMBER,
      role_index: 2,
      joined_at: 't',
    }
    const merged = mergeGuildMember(member, new Map([[MEMBER, 'Online']]))
    expect(merged.status).toBe('Online')
  })

  it('never resolves display name — no lookup endpoint exists yet', () => {
    const member: GuildMemberResponse = {
      guild_id: 'g1',
      identity_id: MEMBER,
      role_index: 2,
      joined_at: 't',
    }
    expect(mergeGuildMember(member, new Map()).displayName).toBeUndefined()
  })
})

describe('groupMembersByRole', () => {
  const roles: RoleResponse[] = [
    { name_index: 0, name: 'owner', permissions: ['manage_guild'] },
    { name_index: 1, name: 'officer', permissions: ['manage_members', 'manage_channels'] },
    { name_index: 2, name: 'member', permissions: [] },
  ]

  const members: GuildMember[] = [
    { identityId: MEMBER, roleIndex: 2, status: 'Offline', joinedAt: 't3' },
    { identityId: OWNER, roleIndex: 0, status: 'Online', joinedAt: 't1' },
    { identityId: OFFICER, roleIndex: 1, status: 'Away', joinedAt: 't2' },
  ]

  it('groups members by role, ordered owner-first', () => {
    const groups = groupMembersByRole(members, roles)
    expect(groups.map((g) => g.roleName)).toEqual(['owner', 'officer', 'member'])
    expect(groups[0].members.map((m) => m.identityId)).toEqual([OWNER])
    expect(groups[1].members.map((m) => m.identityId)).toEqual([OFFICER])
    expect(groups[2].members.map((m) => m.identityId)).toEqual([MEMBER])
  })

  it('omits a role with no current members', () => {
    const groups = groupMembersByRole(
      members.filter((m) => m.roleIndex !== 1),
      roles,
    )
    expect(groups.map((g) => g.roleName)).toEqual(['owner', 'member'])
  })

  it('falls back to a generic label for an unrecognized role index', () => {
    const groups = groupMembersByRole(
      [{ identityId: MEMBER, roleIndex: 5, status: 'Offline', joinedAt: 't' }],
      roles,
    )
    expect(groups[0].roleName).toBe('Role 5')
  })
})

describe('hasGuildPermission', () => {
  it('is always true for the guild owner, regardless of their permission list', () => {
    expect(hasGuildPermission({ owner: OWNER }, OWNER, [], 'manage_guild')).toBe(true)
  })

  it('is true for a non-owner whose permission list includes it', () => {
    expect(hasGuildPermission({ owner: OWNER }, OFFICER, ['manage_members'], 'manage_members')).toBe(
      true,
    )
  })

  it('is false for a non-owner whose permission list lacks it', () => {
    expect(hasGuildPermission({ owner: OWNER }, MEMBER, [], 'manage_members')).toBe(false)
  })
})

describe('permissionsForMember', () => {
  const roles: RoleResponse[] = [
    { name_index: 0, name: 'owner', permissions: ['manage_guild'] },
    { name_index: 1, name: 'officer', permissions: ['manage_members'] },
  ]
  const members: GuildMember[] = [
    { identityId: OFFICER, roleIndex: 1, status: 'Online', joinedAt: 't' },
  ]

  it("resolves the caller's permissions via their role_index", () => {
    expect(permissionsForMember(OFFICER, members, roles)).toEqual(['manage_members'])
  })

  it('returns an empty list for a non-member', () => {
    expect(permissionsForMember('someone-else', members, roles)).toEqual([])
  })
})

describe('roleVariantForIndex', () => {
  it('maps 0/1 to owner/officer and everything else to member', () => {
    expect(roleVariantForIndex(0)).toBe('owner')
    expect(roleVariantForIndex(1)).toBe('officer')
    expect(roleVariantForIndex(2)).toBe('member')
    expect(roleVariantForIndex(7)).toBe('member')
  })
})

describe('canKickMember', () => {
  const guild = { owner: OWNER }
  const officerTarget: GuildMember = { identityId: OFFICER, roleIndex: 1, status: 'Online', joinedAt: 't' }
  const memberTarget: GuildMember = { identityId: MEMBER, roleIndex: 2, status: 'Online', joinedAt: 't' }
  const ownerTarget: GuildMember = { identityId: OWNER, roleIndex: 0, status: 'Online', joinedAt: 't' }

  it('is never true against the owner, regardless of permissions', () => {
    expect(canKickMember(guild, OWNER, ['manage_members', 'manage_roles'], ownerTarget)).toBe(false)
  })

  it('is false without manage_members', () => {
    expect(canKickMember(guild, OFFICER, [], memberTarget)).toBe(false)
  })

  it('manage_members alone is enough to kick a plain member', () => {
    expect(canKickMember(guild, OFFICER, ['manage_members'], memberTarget)).toBe(true)
  })

  it('manage_members alone is NOT enough to kick an elevated-role peer', () => {
    expect(canKickMember(guild, OFFICER, ['manage_members'], officerTarget)).toBe(false)
  })

  it('manage_members + manage_roles can kick an elevated-role member', () => {
    expect(
      canKickMember(guild, OFFICER, ['manage_members', 'manage_roles'], officerTarget),
    ).toBe(true)
  })

  it('the owner can always kick an elevated-role member with just manage_members', () => {
    expect(canKickMember(guild, OWNER, ['manage_members'], officerTarget)).toBe(true)
  })
})

describe('canChangeMemberRole', () => {
  const guild = { owner: OWNER }
  const memberTarget: GuildMember = { identityId: MEMBER, roleIndex: 2, status: 'Online', joinedAt: 't' }
  const ownerTarget: GuildMember = { identityId: OWNER, roleIndex: 0, status: 'Online', joinedAt: 't' }

  it('is never true against the owner', () => {
    expect(canChangeMemberRole(guild, OWNER, ['manage_roles'], ownerTarget)).toBe(false)
  })

  it('requires manage_roles', () => {
    expect(canChangeMemberRole(guild, OFFICER, ['manage_members'], memberTarget)).toBe(false)
    expect(canChangeMemberRole(guild, OFFICER, ['manage_roles'], memberTarget)).toBe(true)
  })

  it('is true for the owner regardless of their permission list', () => {
    expect(canChangeMemberRole(guild, OWNER, [], memberTarget)).toBe(true)
  })
})
