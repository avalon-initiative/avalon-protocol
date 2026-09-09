import { describe, expect, it } from 'vitest'
import {
  addFavoriteGameId,
  buildDiscoverQueryString,
  canChangeMemberRole,
  canKickMember,
  canPinMoreFavorites,
  filterGuildsByNameOrTag,
  filterMembersByIdentityId,
  formatFavoriteGameEntry,
  formatGameBreakdownEntry,
  formatPlayingSummary,
  groupMembersByRole,
  groupMembersPlayingByGame,
  hasGuildPermission,
  hasNoGameBreakdownData,
  MAX_FAVORITE_GAMES,
  membershipStatusText,
  mergeGuildMember,
  permissionsForMember,
  pinnableBreakdownEntries,
  removeFavoriteGameId,
  reorderFavoriteGameIds,
  roleVariantForIndex,
  sortMembers,
  sortMembersByPresence,
} from './guilds'
import type { GuildMember } from './guilds'
import type {
  FavoriteGameEntry,
  GameBreakdownEntry,
  GuildMemberResponse,
  GuildResponse,
  RoleResponse,
} from './types'

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

  it('leaves display name undefined when the profile map has no entry (#161)', () => {
    const member: GuildMemberResponse = {
      guild_id: 'g1',
      identity_id: MEMBER,
      role_index: 2,
      joined_at: 't',
    }
    expect(mergeGuildMember(member, new Map()).displayName).toBeUndefined()
  })

  it('resolves display name from the profile map when present (#161)', () => {
    const member: GuildMemberResponse = {
      guild_id: 'g1',
      identity_id: MEMBER,
      role_index: 2,
      joined_at: 't',
    }
    const merged = mergeGuildMember(member, new Map(), new Map([[MEMBER, 'raid-leader-99']]))
    expect(merged.displayName).toBe('raid-leader-99')
  })

  it('defaults to not-playing (null) when the presence map has no playing entry', () => {
    const member: GuildMemberResponse = {
      guild_id: 'g1',
      identity_id: MEMBER,
      role_index: 2,
      joined_at: 't',
    }
    expect(mergeGuildMember(member, new Map()).playing).toBeNull()
  })

  it('carries the playing game id through when present (#57)', () => {
    const member: GuildMemberResponse = {
      guild_id: 'g1',
      identity_id: MEMBER,
      role_index: 2,
      joined_at: 't',
    }
    const merged = mergeGuildMember(member, new Map(), new Map(), new Map([[MEMBER, 'ashen-realms']]))
    expect(merged.playing).toBe('ashen-realms')
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

describe('membershipStatusText', () => {
  it('says owner when isOwner is true, regardless of isMember', () => {
    expect(membershipStatusText(true, true)).toBe('You are the owner of this guild.')
    expect(membershipStatusText(true, false)).toBe('You are the owner of this guild.')
  })

  it('says member when isMember is true and not the owner', () => {
    expect(membershipStatusText(false, true)).toBe('You are a member.')
  })

  it('says not a member otherwise', () => {
    expect(membershipStatusText(false, false)).toBe('You are not a member of this guild.')
  })
})

describe('sortMembersByPresence', () => {
  it('orders online (and away) members before offline ones', () => {
    const members: GuildMember[] = [
      { identityId: 'a', roleIndex: 2, status: 'Offline', joinedAt: 't' },
      { identityId: 'b', roleIndex: 2, status: 'Online', joinedAt: 't' },
      { identityId: 'c', roleIndex: 2, status: 'Away', joinedAt: 't' },
      { identityId: 'd', roleIndex: 2, status: 'Offline', joinedAt: 't' },
    ]
    expect(sortMembersByPresence(members).map((m) => m.identityId)).toEqual(['b', 'c', 'a', 'd'])
  })
})

describe('filterMembersByIdentityId', () => {
  const members: GuildMember[] = [
    { identityId: 'identity:Alice123', roleIndex: 2, status: 'Online', joinedAt: 't' },
    { identityId: 'identity:bob456', roleIndex: 2, status: 'Online', joinedAt: 't' },
  ]

  it('matches case-insensitively on a partial substring', () => {
    expect(filterMembersByIdentityId(members, 'alice').map((m) => m.identityId)).toEqual([
      'identity:Alice123',
    ])
  })

  it('returns every member for an empty/whitespace query', () => {
    expect(filterMembersByIdentityId(members, '   ')).toEqual(members)
  })

  it('returns no members when nothing matches', () => {
    expect(filterMembersByIdentityId(members, 'nope')).toEqual([])
  })

  it('also matches on a resolved display name (#161)', () => {
    const withNames: GuildMember[] = [
      { identityId: 'identity:1', displayName: 'raid-leader-99', roleIndex: 2, status: 'Online', joinedAt: 't' },
      { identityId: 'identity:2', displayName: 'healer-bot', roleIndex: 2, status: 'Online', joinedAt: 't' },
    ]
    expect(filterMembersByIdentityId(withNames, 'raid').map((m) => m.identityId)).toEqual(['identity:1'])
  })
})

describe('sortMembers', () => {
  const members: GuildMember[] = [
    { identityId: 'zeta', roleIndex: 2, status: 'Online', joinedAt: 't' },
    { identityId: 'alpha', roleIndex: 2, status: 'Online', joinedAt: 't' },
  ]

  it('sorts by identity id when order is "name"', () => {
    expect(sortMembers(members, 'name').map((m) => m.identityId)).toEqual(['alpha', 'zeta'])
  })

  it('leaves order unchanged when order is "role"', () => {
    expect(sortMembers(members, 'role')).toEqual(members)
  })

  it('sorts by resolved display name when present, falling back to id (#161)', () => {
    const mixed: GuildMember[] = [
      { identityId: 'identity:1', displayName: 'zeta-player', roleIndex: 2, status: 'Online', joinedAt: 't' },
      { identityId: 'identity:2', displayName: 'alpha-player', roleIndex: 2, status: 'Online', joinedAt: 't' },
      { identityId: 'identity:3', roleIndex: 2, status: 'Online', joinedAt: 't' },
    ]
    expect(sortMembers(mixed, 'name').map((m) => m.identityId)).toEqual(['identity:2', 'identity:3', 'identity:1'])
  })
})

describe('groupMembersPlayingByGame', () => {
  it('groups by game id, counts descending', () => {
    const members: GuildMember[] = [
      { identityId: 'a', roleIndex: 2, status: 'Online', joinedAt: 't', playing: 'ashen-realms' },
      { identityId: 'b', roleIndex: 2, status: 'Online', joinedAt: 't', playing: 'worldzero' },
      { identityId: 'c', roleIndex: 2, status: 'Online', joinedAt: 't', playing: 'ashen-realms' },
      { identityId: 'd', roleIndex: 2, status: 'Offline', joinedAt: 't', playing: null },
    ]
    expect(groupMembersPlayingByGame(members)).toEqual([
      { gameId: 'ashen-realms', count: 2 },
      { gameId: 'worldzero', count: 1 },
    ])
  })

  it('omits members with no playing value entirely, rather than a null bucket', () => {
    const members: GuildMember[] = [
      { identityId: 'a', roleIndex: 2, status: 'Online', joinedAt: 't', playing: null },
      { identityId: 'b', roleIndex: 2, status: 'Offline', joinedAt: 't' },
    ]
    expect(groupMembersPlayingByGame(members)).toEqual([])
  })

  it('breaks ties alphabetically by game id', () => {
    const members: GuildMember[] = [
      { identityId: 'a', roleIndex: 2, status: 'Online', joinedAt: 't', playing: 'worldzero' },
      { identityId: 'b', roleIndex: 2, status: 'Online', joinedAt: 't', playing: 'ashen-realms' },
    ]
    expect(groupMembersPlayingByGame(members).map((g) => g.gameId)).toEqual(['ashen-realms', 'worldzero'])
  })
})

describe('formatPlayingSummary', () => {
  it('uses the #74-safe "N members playing X" phrasing, never "Game X\'s guild"', () => {
    expect(formatPlayingSummary({ gameId: 'Ashen Realms', count: 42 })).toBe('42 members playing Ashen Realms')
  })

  it('uses singular "member" for a count of one', () => {
    expect(formatPlayingSummary({ gameId: 'WorldZero', count: 1 })).toBe('1 member playing WorldZero')
  })
})

describe('filterGuildsByNameOrTag', () => {
  const guilds: Pick<GuildResponse, 'name' | 'tag'>[] = [
    { name: 'Ashen Vanguard', tag: 'ASHV' },
    { name: 'Twilight Order', tag: 'TWIL' },
  ]

  it('matches case-insensitively on a partial name substring', () => {
    expect(filterGuildsByNameOrTag(guilds, 'ashen')).toEqual([guilds[0]])
  })

  it('matches case-insensitively on a partial tag substring', () => {
    expect(filterGuildsByNameOrTag(guilds, 'twil')).toEqual([guilds[1]])
  })

  it('returns every guild for an empty query', () => {
    expect(filterGuildsByNameOrTag(guilds, '')).toEqual(guilds)
  })

  it('returns no guilds when nothing matches', () => {
    expect(filterGuildsByNameOrTag(guilds, 'nope')).toEqual([])
  })
})

describe('buildDiscoverQueryString', () => {
  it('returns an empty string when every param is omitted', () => {
    expect(buildDiscoverQueryString({})).toBe('')
  })

  it('includes only the params that were provided', () => {
    expect(buildDiscoverQueryString({ q: 'dragons' })).toBe('?q=dragons')
  })

  it('omits a blank/whitespace-only q rather than sending an empty value', () => {
    expect(buildDiscoverQueryString({ q: '   ' })).toBe('')
  })

  it('trims q and tag before encoding', () => {
    expect(buildDiscoverQueryString({ q: '  dragons  ' })).toBe('?q=dragons')
    expect(buildDiscoverQueryString({ tag: '  ASHV  ' })).toBe('?tag=ASHV')
  })

  it('encodes an explicit recruiting=false, distinct from omitting it', () => {
    expect(buildDiscoverQueryString({ recruiting: false })).toBe('?recruiting=false')
    expect(buildDiscoverQueryString({ recruiting: true })).toBe('?recruiting=true')
    expect(buildDiscoverQueryString({})).not.toContain('recruiting')
  })

  it('combines every filter into one query string', () => {
    const query = buildDiscoverQueryString({
      q: 'dragons',
      recruiting: true,
      tag: 'ASHV',
      game: 'game-1',
      sort: 'alphabetical',
      limit: 10,
      cursor: 'guild-9',
    })
    const params = new URLSearchParams(query.slice(1))
    expect(params.get('q')).toBe('dragons')
    expect(params.get('recruiting')).toBe('true')
    expect(params.get('tag')).toBe('ASHV')
    expect(params.get('game')).toBe('game-1')
    expect(params.get('sort')).toBe('alphabetical')
    expect(params.get('limit')).toBe('10')
    expect(params.get('cursor')).toBe('guild-9')
  })
})

describe('formatGameBreakdownEntry', () => {
  it('renders "N of M members play <game>" verbatim', () => {
    const entry: GameBreakdownEntry = {
      game_id: 'game-1',
      game_slug: 'ashen-realms',
      game_name: 'Ashen Realms',
      member_count: 14,
    }
    expect(formatGameBreakdownEntry(entry, 22)).toBe('14 of 22 members play Ashen Realms')
  })

  it('total_members is the denominator, not a sum of member_count entries', () => {
    // A member can be bound to zero, one, or several games, so
    // total_members (guild membership) and a single entry's member_count
    // are independent numbers — this just pins that the function uses the
    // total passed in, not anything derived from the entry itself.
    const entry: GameBreakdownEntry = {
      game_id: 'game-2',
      game_slug: 'ocean-world',
      game_name: 'Ocean World',
      member_count: 3,
    }
    expect(formatGameBreakdownEntry(entry, 3)).toBe('3 of 3 members play Ocean World')
  })
})

describe('hasNoGameBreakdownData', () => {
  it('is true for an empty breakdown', () => {
    expect(hasNoGameBreakdownData([])).toBe(true)
  })

  it('is false once at least one game has a bound member', () => {
    const entry: GameBreakdownEntry = {
      game_id: 'game-1',
      game_slug: 'ashen-realms',
      game_name: 'Ashen Realms',
      member_count: 1,
    }
    expect(hasNoGameBreakdownData([entry])).toBe(false)
  })
})

// --- Issue #207: favorite games pin list -----------------------------------

function favorite(gameId: string, name: string, position: number, stale = false): FavoriteGameEntry {
  return { game_id: gameId, game_slug: gameId, game_name: name, position, stale }
}

function breakdownEntry(gameId: string, name: string): GameBreakdownEntry {
  return { game_id: gameId, game_slug: gameId, game_name: name, member_count: 1 }
}

describe('pinnableBreakdownEntries', () => {
  it('excludes games already pinned', () => {
    const breakdown = [breakdownEntry('g1', 'Ashen Realms'), breakdownEntry('g2', 'Ocean World')]
    const favorites = [favorite('g1', 'Ashen Realms', 0)]
    expect(pinnableBreakdownEntries(breakdown, favorites)).toEqual([breakdownEntry('g2', 'Ocean World')])
  })

  it('returns every breakdown entry when nothing is pinned yet', () => {
    const breakdown = [breakdownEntry('g1', 'Ashen Realms')]
    expect(pinnableBreakdownEntries(breakdown, [])).toEqual(breakdown)
  })
})

describe('canPinMoreFavorites', () => {
  it('allows pinning below the cap', () => {
    const favorites = [favorite('g1', 'A', 0), favorite('g2', 'B', 1)]
    expect(canPinMoreFavorites(favorites)).toBe(true)
  })

  it('rejects pinning at the cap', () => {
    const favorites = Array.from({ length: MAX_FAVORITE_GAMES }, (_, i) => favorite(`g${i}`, `Game ${i}`, i))
    expect(favorites.length).toBe(5)
    expect(canPinMoreFavorites(favorites)).toBe(false)
  })
})

describe('addFavoriteGameId', () => {
  it('appends the new id to the existing ordered ids', () => {
    const favorites = [favorite('g1', 'A', 0), favorite('g2', 'B', 1)]
    expect(addFavoriteGameId(favorites, 'g3')).toEqual(['g1', 'g2', 'g3'])
  })
})

describe('removeFavoriteGameId', () => {
  it('drops the given id, preserving the order of the rest', () => {
    const favorites = [favorite('g1', 'A', 0), favorite('g2', 'B', 1), favorite('g3', 'C', 2)]
    expect(removeFavoriteGameId(favorites, 'g2')).toEqual(['g1', 'g3'])
  })
})

describe('reorderFavoriteGameIds', () => {
  const favorites = [favorite('g1', 'A', 0), favorite('g2', 'B', 1), favorite('g3', 'C', 2)]

  it('moves an entry up by swapping with its predecessor', () => {
    expect(reorderFavoriteGameIds(favorites, 'g2', 'up')).toEqual(['g2', 'g1', 'g3'])
  })

  it('moves an entry down by swapping with its successor', () => {
    expect(reorderFavoriteGameIds(favorites, 'g2', 'down')).toEqual(['g1', 'g3', 'g2'])
  })

  it('is a no-op moving the first entry up', () => {
    expect(reorderFavoriteGameIds(favorites, 'g1', 'up')).toEqual(['g1', 'g2', 'g3'])
  })

  it('is a no-op moving the last entry down', () => {
    expect(reorderFavoriteGameIds(favorites, 'g3', 'down')).toEqual(['g1', 'g2', 'g3'])
  })

  it('is a no-op for an id not in the list', () => {
    expect(reorderFavoriteGameIds(favorites, 'missing', 'up')).toEqual(['g1', 'g2', 'g3'])
  })
})

describe('formatFavoriteGameEntry', () => {
  it('renders the plain game name when not stale', () => {
    expect(formatFavoriteGameEntry(favorite('g1', 'Ashen Realms', 0))).toBe('Ashen Realms')
  })

  it('flags staleness inline rather than hiding it', () => {
    expect(formatFavoriteGameEntry(favorite('g1', 'Ashen Realms', 0, true))).toBe(
      'Ashen Realms (no longer actively played)',
    )
  })
})
