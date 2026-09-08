import type { PresenceStatus } from './AvalonPresenceBadge.types'

export interface AvalonFriendRowProps {
  identityId: string
  // Always undefined today — see apps/hub/src/api/friends.ts's Friend type
  // for why (no profile-lookup-by-id endpoint exists yet).
  displayName?: string
  status: PresenceStatus
}
