import type { PresenceStatus } from './AvalonPresenceBadge.types'

export interface AvalonFriendRowProps {
  identityId: string
  // Always undefined today — see apps/hub/src/api/friends.ts's Friend type
  // for why (no profile-lookup-by-id endpoint exists yet).
  displayName?: string
  // Same gap as displayName: no other identity's avatar is resolvable yet.
  avatarUrl?: string | null
  status: PresenceStatus
}
