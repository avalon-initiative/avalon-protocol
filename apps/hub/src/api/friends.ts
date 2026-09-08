// Orchestrates friends + presence + display names for issue #18: fetches
// all three separately and merges them client-side, since GET /friends
// embeds neither presence nor a display name server-side (see
// crates/server/src/friends.rs — FriendshipResponse has no such fields).
// Mirrors crates/sdk/src/social.rs's Session::friends()/merge_friend in
// Rust — same shape, ported to TypeScript for the Hub, which doesn't
// consume the Rust SDK directly. Display names are resolved via
// GET /identities/profiles (issue #161).
import * as api from './client'
import type {
  FriendRequestResponse,
  FriendshipResponse,
  PresenceResponse,
  PresenceStatus,
  PublicProfileResponse,
} from './types'

export interface Friend {
  identityId: string
  // Undefined only if #161's batch lookup has no profile for this id
  // (shouldn't happen for a real friend, but the UI still falls back to
  // the raw id rather than assuming).
  displayName?: string
  status: PresenceStatus
  since: string
}

// The other party in a friendship pair, given the caller's own id.
function otherParty(friendship: FriendshipResponse, selfId: string): string {
  return friendship.a === selfId ? friendship.b : friendship.a
}

// Pure merge logic, testable without any network call — mirrors
// crates/sdk/src/social.rs's `merge_friend`. A missing presence entry
// (not present in `presenceByStatus`) defaults to Offline rather than
// guessing at "last seen" — see #78, presence honesty.
export function mergeFriend(
  friendship: FriendshipResponse,
  selfId: string,
  presenceByStatus: Map<string, PresenceStatus>,
  displayNameById: Map<string, string> = new Map(),
): Friend {
  const identityId = otherParty(friendship, selfId)
  return {
    identityId,
    displayName: displayNameById.get(identityId),
    status: presenceByStatus.get(identityId) ?? 'Offline',
    since: friendship.since,
  }
}

export async function listFriendsWithPresence(token: string, selfId: string): Promise<Friend[]> {
  const friendships = await api.listFriends(token)
  if (friendships.length === 0) {
    return []
  }

  const otherIds = friendships.map((f) => otherParty(f, selfId))
  const [presences, profiles] = await Promise.all([
    api.getPresence(token, otherIds),
    api.getProfiles(token, otherIds),
  ])
  const presenceByStatus = new Map<string, PresenceStatus>(
    presences.map((p: PresenceResponse) => [p.identity_id, p.status]),
  )
  const displayNameById = new Map<string, string>(
    profiles.map((p: PublicProfileResponse) => [p.identity_id, p.display_name]),
  )

  return friendships.map((f) => mergeFriend(f, selfId, presenceByStatus, displayNameById))
}

export interface FriendRequestView {
  id: string
  otherIdentityId: string
  direction: 'incoming' | 'outgoing'
  requestedAt: string
}

export function splitFriendRequests(
  requests: FriendRequestResponse[],
  selfId: string,
): FriendRequestView[] {
  return requests.map((r) => ({
    id: r.id,
    direction: r.from === selfId ? 'outgoing' : 'incoming',
    otherIdentityId: r.from === selfId ? r.to : r.from,
    requestedAt: r.requested_at,
  }))
}
