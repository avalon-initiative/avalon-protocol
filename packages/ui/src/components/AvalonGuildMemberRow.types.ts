import type { PresenceStatus } from './AvalonPresenceBadge.types'
import type { AvalonRoleBadgeProps } from './AvalonRoleBadge.types'

export interface AvalonGuildMemberRowProps {
  identityId: string
  // Always undefined today — no endpoint resolves another identity's
  // display name yet, same gap AvalonFriendRow already documents (tracked
  // as #161, "Batch identity lookup: resolve display names for
  // friends/guild rosters"). The component falls back to a shortened
  // identity id when this is unset — see AvalonGuildMemberRow.vue.
  displayName?: string
  avatarUrl?: string | null
  status: PresenceStatus
  roleName: string
  roleVariant?: AvalonRoleBadgeProps['variant']
  // Separate flags rather than one `canManage` — the server gates them on
  // different permissions (`manage_roles` for a role change,
  // `manage_members` — plus `manage_roles` again for a non-member-tier
  // target — for a kick; see crates/server/src/guilds.rs). Decided by the
  // app from the caller's own permissions (issue #24's "hide the button,
  // don't crash on a 403 anyway" invariant), never by this component.
  canChangeRole?: boolean
  canKick?: boolean
}
