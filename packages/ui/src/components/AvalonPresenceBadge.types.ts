// Its own copy of the status union rather than importing an app's wire
// type — packages/ui knows nothing about fetches, tokens, or any specific
// app's API shape (issue #18's invariant), so this stays a plain prop type
// a caller maps their own data onto.
export type PresenceStatus = 'Online' | 'Away' | 'Offline'

export interface AvalonPresenceBadgeProps {
  status: PresenceStatus
}
