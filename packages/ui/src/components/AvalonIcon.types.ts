export type AvalonIconName =
  | 'home'
  | 'games'
  | 'guilds'
  | 'friends'
  | 'chat'
  | 'discover'
  | 'profile'
  | 'search'
  | 'bell'
  | 'plus'
  | 'device'
  | 'activity'
  | 'logo'
  | 'alert'
  | 'pencil'
  | 'check'
  | 'close'
  // Added for issue #311 (IconMock.png's full icon set) — grouped to match
  // that sheet's own sections.
  // Core navigation & features
  | 'settings'
  | 'voice'
  | 'video'
  | 'messages'
  | 'calendar'
  | 'achievements'
  | 'library'
  | 'wallet'
  | 'more'
  // Game & community
  | 'community'
  | 'faction'
  | 'event'
  | 'reward'
  | 'leaderboards'
  | 'map'
  // Status & interaction
  | 'join'
  | 'leave'
  | 'invite'
  | 'share'
  | 'bookmark'
  | 'follow'
  | 'muted'
  | 'block'

export interface AvalonIconProps {
  name: AvalonIconName
  /** Rendered size in CSS pixels; the SVG scales, so any value works. */
  size?: number
}
