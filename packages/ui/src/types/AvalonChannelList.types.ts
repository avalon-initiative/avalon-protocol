export interface AvalonChannelListItem {
  id: string
  name: string
  archived: boolean
}

export interface AvalonChannelListProps {
  channels: AvalonChannelListItem[]
  activeChannelId?: string
  // Whether the caller may create/archive channels — decided by the app
  // from its own permission check (issue #24's UI-gating invariant), never
  // by this component.
  canManage?: boolean
}
