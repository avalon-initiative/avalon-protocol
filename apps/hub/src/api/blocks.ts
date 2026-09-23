// Orchestrates GET /blocks + display-name resolution for issue #97's
// "Blocked users" list — mirrors friends.ts's listFriendsWithPresence
// pattern: GET /blocks embeds no display name server-side
// (crates/server/src/blocks.rs's Block has only `blocked`/`createdAt`),
// so it's resolved client-side via the same batch GET /identities/profiles
// lookup.
import type { AccountSession, Block } from '@avalon-initiative/protocol-sdk'

export interface BlockedUser {
  identityId: string
  // Undefined only if the batch lookup has no profile for this id.
  displayName?: string
  since: string
}

export function mergeBlockedUser(entry: Block, displayNameById: Map<string, string> = new Map()): BlockedUser {
  return {
    identityId: entry.blocked,
    displayName: displayNameById.get(entry.blocked),
    since: entry.createdAt,
  }
}

export async function listBlockedUsersWithNames(session: AccountSession): Promise<BlockedUser[]> {
  const entries = await session.blocks()
  if (!Array.isArray(entries) || entries.length === 0) {
    return []
  }

  const profiles = await session.profiles(entries.map((e) => e.blocked))
  const displayNameById = new Map<string, string>(
    (Array.isArray(profiles) ? profiles : []).map((p) => [p.identityId, p.displayName]),
  )

  return entries.map((e) => mergeBlockedUser(e, displayNameById))
}
