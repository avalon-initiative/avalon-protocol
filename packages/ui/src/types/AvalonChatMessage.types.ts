export interface AvalonChatMessageProps {
  authorId: string
  // Resolved via GET /identities/profiles (issue #161) by the caller,
  // e.g. useGuildChat. Falls back to authorId when unresolved (a fresh
  // author this session hasn't looked up yet, or the lookup failed).
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
