import type { PresenceStatus } from './AvalonPresenceBadge.types'

export interface AvalonChatMessageProps {
  authorId: string
  // The author's current presence — resolved by the caller (guild roster
  // presence, or a live subscription for DM participants), omitted/absent
  // when unknown rather than guessed at here. Drives a small status dot on
  // the avatar; never shown for the viewer's own messages (isOwn), since a
  // reader always knows their own status.
  presenceStatus?: PresenceStatus
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
  // Whether the viewing session authored this message — decided by the
  // caller (authorId === selfId), never guessed here. Drives the
  // iMessage-style right/left alignment and bubble tone.
  isOwn?: boolean
}
