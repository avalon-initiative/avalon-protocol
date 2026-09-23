export interface AvalonRoleBadgeProps {
  name: string
  // The caller (e.g. AvalonGuildMemberRow) decides this from the role's
  // name_index — this component knows nothing about guild wire shapes
  // matching AvalonPresenceBadge's own precedent
  // of never importing an app-specific type).
  variant?: 'owner' | 'officer' | 'member'
}
