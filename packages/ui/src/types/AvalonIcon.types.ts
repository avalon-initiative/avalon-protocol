export type AvalonIconName =
  | 'home'
  | 'integrators'
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
  // Integrator & community
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
  // Achievement/milestone icons (issue #332) — the built-in set an
  // AvalonAchievementCard falls back to when a definition has no
  // integrator-hosted `icon_url`. See AchievementIconName in
  // AvalonAchievementCard.types.ts, which mirrors this same fixed-key
  // pattern for that narrower set.
  | 'trophy'
  | 'star'
  | 'shield'
  | 'sword'
  // Role badge icons (issue #152's RoleBadgeIcon vocabulary, #462) — the
  // remaining 5 of that fixed 8-icon set not already covered above.
  | 'crown'
  | 'wrench'
  | 'heart'
  | 'flag'
  | 'bolt'

export interface AvalonIconProps {
  name: AvalonIconName
  /** Rendered size in CSS pixels; the SVG scales, so any value works. */
  size?: number
}
