export interface AvalonSuggestionRowProps {
  identityId: string
  // Undefined only if the batch profile lookup has no entry for this id —
  // same fallback-to-raw-id posture AvalonFriendRow already documents.
  displayName?: string
  avatarUrl?: string | null
  // True once an add-friend request has just been sent for this
  // suggestion, so the row can swap its button to a disabled "Requested"
  // state instead of disappearing outright (issue #204).
  requested?: boolean
}
