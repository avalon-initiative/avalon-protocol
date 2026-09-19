// "People you may know" (issue #204) — fetches GET /people/discover, then
// resolves display names for the returned candidates via the same batch
// GET /identities/profiles lookup api/friends.ts's listFriendsWithPresence
// already uses. A missing profile falls back to the raw identity id,
// same posture as Friend.displayName.
import * as api from '@avalon/api-client'
import type { DiscoverPeopleResponse, PublicProfileResponse } from '@avalon/api-client'

export interface Suggestion {
  identityId: string
  displayName?: string
}

export function mergeSuggestion(
  identityId: string,
  displayNameById: Map<string, string>,
): Suggestion {
  return {
    identityId,
    displayName: displayNameById.get(identityId),
  }
}

export async function listSuggestions(token: string): Promise<Suggestion[]> {
  const response: DiscoverPeopleResponse = await api.discoverPeople(token)
  const ids = response.candidates.map((c) => c.identity_id)
  if (ids.length === 0) {
    return []
  }

  const profiles = await api.getProfiles(token, ids)
  const displayNameById = new Map<string, string>(
    profiles.map((p: PublicProfileResponse) => [p.identity_id, p.display_name]),
  )

  return ids.map((id) => mergeSuggestion(id, displayNameById))
}
