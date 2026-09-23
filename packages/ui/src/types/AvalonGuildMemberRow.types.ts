import type { PresenceStatus } from './AvalonPresenceBadge.types'
import type { AvalonRoleBadgeProps } from './AvalonRoleBadge.types'

export interface AvalonGuildMemberRowProps {
  identityId: string
  // Resolved via GET /identities/profiles by
  // listMembersWithPresence. Undefined only if that lookup has no
  // profile for this id (shouldn't happen for a real member) or the
  // caller skipped it — the component falls back to a shortened
  // identity id when unset, see AvalonGuildMemberRow.vue.
  displayName?: string
  avatarUrl?: string | null
  status: PresenceStatus
  roleName: string
  roleVariant?: AvalonRoleBadgeProps['variant']
  // Separate flags rather than one `canManage` — the server gates them on
  // different permissions (`manage_roles` for a role change,
  // `manage_members` — plus `manage_roles` again for a non-member-tier
  // target — for a kick; see crates/server/src/guilds.rs). Decided by the
  // app from the caller's own permissions ("hide the button,
  // don't crash on a 403 anyway" invariant), never by this component.
  canChangeRole?: boolean
  canKick?: boolean
}
