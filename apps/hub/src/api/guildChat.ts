// Pure guild-chat logic (issue #24) kept separate from apps/hub/src/api/guilds.ts
// since it's about message composition/ordering rather than roster/role
// merging — following the ticket's own suggested split
// (guilds.ts / guildChat.ts "or similar").
import type { MessageResponse } from './types'

// Matches crates/server/src/guild_messages.rs::MESSAGE_BODY_MAX_CHARS
// exactly — surfaced here so the composer can show a live counter/hard
// stop instead of only learning the cap from a rejected request.
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
// DESC). The chat view renders newest-at-bottom, so a page fetched for
// "load older" needs reversing before it's prepended to the visible list.
// Pure so the ordering logic doesn't need a component/DOM to test.
export function toOldestFirst(messages: MessageResponse[]): MessageResponse[] {
  return [...messages].reverse()
}
