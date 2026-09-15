// Pure guild-chat logic (issue #24) kept separate from apps/hub/src/api/guilds.ts
// since it's about message composition/ordering rather than roster/role
// merging — following the ticket's own suggested split
// (guilds.ts / guildChat.ts "or similar").

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

// GET .../messages (and .../messages/archive, issue #464 — same
// newest-first shape) returns newest-first (server: ORDER BY sent_at
// DESC, id DESC). The chat view renders newest-at-bottom, so a page
// fetched for "load older" needs reversing before it's prepended to the
// visible list. Pure, and generic since it never touches message fields,
// so it works for either response shape.
export function toOldestFirst<T>(messages: T[]): T[] {
  return [...messages].reverse()
}
