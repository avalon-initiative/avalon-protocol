export interface AvalonChatMessageProps {
  authorId: string
  // Always undefined today — no endpoint resolves another identity's
  // display name yet, same gap AvalonFriendRow already documents.
  authorDisplayName?: string
  body: string
  // A pre-formatted, caller-supplied time string (e.g. "3:04 PM") — this
  // component does no date formatting of its own, matching the "props
  // only, no logic" rule.
  sentAtLabel: string
  // Whether the caller may hard-delete this message — decided by the app
  // (issue #24's UI-gating invariant), never by this component.
  canDelete?: boolean
}
