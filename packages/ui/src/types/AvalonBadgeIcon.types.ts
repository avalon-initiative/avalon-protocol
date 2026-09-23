export type AvalonBadgeTier =
  | 'achievement'
  | 'rare'
  | 'epic'
  | 'legendary'
  | 'event'
  | 'rank'
  | 'guild'
  | 'special'

export interface AvalonBadgeIconProps {
  tier: AvalonBadgeTier
  /** Rendered size in CSS pixels; the frame scales, so any value works. */
  size?: number
}
