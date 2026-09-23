export interface AvalonChatComposerProps {
  modelValue: string
  // Server's body-length cap (crates/server/src/guild_messages.rs's
  // MESSAGE_BODY_MAX_CHARS) — passed in rather than hardcoded, so this
  // component stays wire-shape-agnostic.
  maxChars: number
  sending?: boolean
  error?: string
  disabled?: boolean
}
