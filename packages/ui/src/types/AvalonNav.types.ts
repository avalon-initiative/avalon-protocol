import type { AvalonIconName } from './AvalonIcon.types'

/** One navigation entry, shared by the sidebar (desktop) and bottom nav (mobile). */
export interface AvalonNavItem {
  label: string
  to: string
  icon: AvalonIconName
  active: boolean
  /** Renders muted and non-interactive with a "Soon" tag — for features that don't exist yet. */
  disabled?: boolean
}

export interface AvalonNavProps {
  items: AvalonNavItem[]
}
