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

export interface AvalonIconProps {
  name: AvalonIconName
  /** Rendered size in CSS pixels; the SVG scales, so any value works. */
  size?: number
}
