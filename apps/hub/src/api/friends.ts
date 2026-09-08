// Orchestrates friends + presence for issue #18: fetches both separately
// and merges them client-side, since GET /friends does not embed presence
// server-side (see crates/server/src/friends.rs — FriendshipResponse has no
// presence field). Mirrors crates/sdk/src/social.rs's Session::friends()/
// merge_friend in Rust — same shape, ported to TypeScript for the Hub,
// which doesn't consume the Rust SDK directly.
import * as api from './client'
import type { FriendRequestResponse, FriendshipResponse, PresenceResponse, PresenceStatus } from './types'

export interface Friend {
  identityId: string
  // Always undefined today — no endpoint resolves another identity's
  // display name yet (see #18's correction note, same gap #17's SDK
  // already documented for Friend.display_name).
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
): Friend {
  const identityId = otherParty(friendship, selfId)
  return {
    identityId,
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
  const presences = await api.getPresence(token, otherIds)
  const presenceByStatus = new Map<string, PresenceStatus>(
    presences.map((p: PresenceResponse) => [p.identity_id, p.status]),
  )

  return friendships.map((f) => mergeFriend(f, selfId, presenceByStatus))
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
