import type { AvalonIconName } from '../types/AvalonIcon.types'
import type { AvalonBadgeTier } from '../types/AvalonBadgeIcon.types'

export const BADGE_GLYPHS: Record<AvalonBadgeTier, AvalonIconName> = {
  achievement: 'star',
  rare: 'bolt',
  epic: 'reward',
  legendary: 'crown',
  event: 'event',
  rank: 'leaderboards',
  guild: 'guilds',
  special: 'heart',
}
