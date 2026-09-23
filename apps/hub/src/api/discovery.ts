// "People you may know" — fetches GET /people/discover, then
// resolves display names for the returned candidates via the same batch
// GET /identities/profiles lookup api/friends.ts's listFriendsWithPresence
// already uses. A missing profile falls back to the raw identity id,
// same posture as Friend.displayName.
import type { AccountSession } from '@avalon/sdk'

export interface Suggestion {
  identityId: string
  displayName?: string
}

export function mergeSuggestion(identityId: string, displayNameById: Map<string, string>): Suggestion {
  return {
    identityId,
    displayName: displayNameById.get(identityId),
  }
}

export async function listSuggestions(session: AccountSession): Promise<Suggestion[]> {
  const candidates = await session.discoverPeople()
  const ids = Array.isArray(candidates) ? candidates.map((c) => c.identityId) : []
  if (ids.length === 0) {
    return []
  }

  const profiles = await session.profiles(ids)
  const displayNameById = new Map<string, string>(
    (Array.isArray(profiles) ? profiles : []).map((p) => [p.identityId, p.displayName]),
  )

  return ids.map((id) => mergeSuggestion(id, displayNameById))
}
