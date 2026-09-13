// Its own copy of the status union rather than importing an app's wire
// type — packages/ui knows nothing about fetches, tokens, or any specific
// app's API shape (issue #18's invariant), so this stays a plain prop type
// a caller maps their own data onto.
export type PresenceStatus = 'Online' | 'Away' | 'DoNotDisturb' | 'Offline'

export interface AvalonPresenceBadgeProps {
  status: PresenceStatus
}

// Display label for a status — `DoNotDisturb` has no space in the wire
// value, so it needs a mapping rather than rendering raw.
export const PRESENCE_STATUS_LABELS: Record<PresenceStatus, string> = {
  Online: 'Online',
  Away: 'Away',
  DoNotDisturb: 'Do Not Disturb',
  Offline: 'Offline',
}
