// Pure direct-message logic (issue #105), kept separate from
// apps/hub/src/api/client.ts for the same reason guildChat.ts is split out
// from guilds.ts — composition/ordering, not roster/relationship merging.
import type { ConversationMessageResponse, ConversationResponse } from './types'

// Matches crates/server/src/conversations.rs::MESSAGE_BODY_MAX_CHARS
// exactly — same cap as guild chat (guildChat.ts's own constant), kept as
// a separate constant since the two server-side caps are independently
// defined and happen to agree, not the same value by construction.
export const MESSAGE_BODY_MAX_CHARS = 4000

export interface ComposerValidation {
  valid: boolean
  charCount: number
  remaining: number
}

// Pure, unit-testable independent of any component — mirrors the server's
// own validate_message_body (trim-empty is also rejected there).
export function validateComposerBody(body: string): ComposerValidation {
  const charCount = body.length
  const trimmedEmpty = body.trim().length === 0
  return {
    valid: !trimmedEmpty && charCount <= MESSAGE_BODY_MAX_CHARS,
    charCount,
    remaining: MESSAGE_BODY_MAX_CHARS - charCount,
  }
}

// GET .../messages returns newest-first (server: ORDER BY sent_at DESC, id
// DESC) — same reversal guildChat.ts's toOldestFirst does, for the same
// newest-at-bottom rendering.
export function toOldestFirst(
  messages: ConversationMessageResponse[],
): ConversationMessageResponse[] {
  return [...messages].reverse()
}

// The other participant(s) in a conversation, from the caller's own point
// of view — excludes `selfId` so a 1:1 conversation's list/header renders
// the friend's name, not the caller's own. A group conversation (more than
// one other participant) returns all of them; callers decide how to
// summarize that (e.g. joining names), this stays pure id-list logic.
export function otherParticipants(conversation: ConversationResponse, selfId: string): string[] {
  return conversation.participants.filter((id) => id !== selfId)
}
