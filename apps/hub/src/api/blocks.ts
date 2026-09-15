// Orchestrates GET /blocks + display-name resolution for issue #97's
// "Blocked users" list — mirrors friends.ts's listFriendsWithPresence
// pattern: GET /blocks embeds no display name server-side
// (crates/server/src/blocks.rs's BlockListEntry has only `blocked`/
// `created_at`), so it's resolved client-side via the same batch
// GET /identities/profiles lookup.
import * as api from './client'
import type { BlockListEntry, PublicProfileResponse } from './types'

export interface BlockedUser {
  identityId: string
  // Undefined only if the batch lookup has no profile for this id.
  displayName?: string
  since: string
}

export function mergeBlockedUser(
  entry: BlockListEntry,
  displayNameById: Map<string, string> = new Map(),
): BlockedUser {
  return {
    identityId: entry.blocked,
    displayName: displayNameById.get(entry.blocked),
    since: entry.created_at,
  }
}

export async function listBlockedUsersWithNames(token: string): Promise<BlockedUser[]> {
  const entries = await api.listBlocks(token)
  if (!Array.isArray(entries) || entries.length === 0) {
    return []
  }

  const profiles = await api.getProfiles(
    token,
    entries.map((e) => e.blocked),
  )
  const displayNameById = new Map<string, string>(
    profiles.map((p: PublicProfileResponse) => [p.identity_id, p.display_name]),
  )

  return entries.map((e) => mergeBlockedUser(e, displayNameById))
}
