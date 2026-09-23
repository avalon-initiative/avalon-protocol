// Turns a raw HistoryEntryResponse into something an actual
// user would want to read. Kept as pure functions, testable
// without mounting Activity.vue: given an event, what should the feed say.
import type { HistoryEntry } from '@avalon/sdk'

// `payload` comes back as `unknown` — every accessor here checks its own
// shape rather than assuming, so a malformed or future-shaped payload never
// crashes the feed, it just falls through to the kind-only fallback below.
function stringField(payload: unknown, key: string): string | undefined {
  if (payload && typeof payload === 'object' && key in payload) {
    const value = (payload as Record<string, unknown>)[key]
    if (typeof value === 'string') return value
  }
  return undefined
}

/**
 * A human-readable summary for one event kind. Unknown/future kinds
 * (this list will keep growing as more event kinds ship) fall back to
 * the raw `kind` string rather than throwing or rendering nothing.
 */
export function summarizeActivityEntry(entry: HistoryEntry): string {
  switch (entry.kind) {
    case 'identity.created':
      return 'You created your identity.'
    case 'friend.requested':
      return 'You sent a friend request.'
    case 'friend.accepted':
      return 'You accepted a friend request.'
    case 'friend.removed':
      return 'You removed a friend.'
    case 'identity.signing_key_added': {
      const label = stringField(entry.payload, 'device_label')
      return label ? `New device added: ${label}.` : 'New device added.'
    }
    case 'identity.signing_key_revoked':
      return 'Device access revoked.'
    // Guild events — /me/history is issuer-filtered to the
    // caller's own events only (see crates/server/src/handlers.rs::my_history),
    // so every one of these is something the caller themselves did.
    case 'guild.created': {
      const name = stringField(entry.payload, 'name')
      return name ? `You created the guild ${name}.` : 'You created a guild.'
    }
    case 'guild.updated': {
      const name = stringField(entry.payload, 'name')
      return name ? `You updated ${name}'s settings.` : 'You updated guild settings.'
    }
    case 'guild.role_defined': {
      const name = stringField(entry.payload, 'name')
      return name ? `You defined the role ${name}.` : 'You defined a guild role.'
    }
    case 'guild.role_deleted':
      return 'You deleted a guild role.'
    case 'guild.role_changed':
      return "You changed a member's role."
    case 'guild.owner_transferred':
      return 'You transferred guild ownership.'
    case 'guild.game_associated':
      return 'You associated an integrator with your guild.'
    case 'guild.member_added':
      return 'You joined a guild.'
    case 'guild.member_removed': {
      const reason = stringField(entry.payload, 'reason')
      return reason === 'removed' ? 'You removed a member from a guild.' : 'You left a guild.'
    }
    case 'guild.favorite_games_updated':
      return "You updated your guild's favorite integrators."
    case 'guild.channel_created': {
      const name = stringField(entry.payload, 'name')
      return name ? `You created the channel #${name}.` : 'You created a guild channel.'
    }
    case 'guild.channel_renamed': {
      const name = stringField(entry.payload, 'name')
      return name ? `You renamed a channel to #${name}.` : 'You renamed a guild channel.'
    }
    case 'guild.channel_archived':
      return 'You archived a guild channel.'
    default:
      return entry.kind
  }
}

const RELATIVE_UNITS: [Intl.RelativeTimeFormatUnit, number][] = [
  ['year', 1000 * 60 * 60 * 24 * 365],
  ['month', 1000 * 60 * 60 * 24 * 30],
  ['week', 1000 * 60 * 60 * 24 * 7],
  ['day', 1000 * 60 * 60 * 24],
  ['hour', 1000 * 60 * 60],
  ['minute', 1000 * 60],
]

const relativeFormatter = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' })

/**
 * "3 hours ago" instead of a raw ISO string. Falls back to "just now" for
 * anything under a minute, and to a locale date string once it's more than
 * a year out — relative phrasing stops being useful at that distance.
 */
export function formatActivityTimestamp(timestamp: string, now: Date = new Date()): string {
  const then = new Date(timestamp)
  const diffMs = then.getTime() - now.getTime()
  const absMs = Math.abs(diffMs)

  for (const [unit, unitMs] of RELATIVE_UNITS) {
    if (absMs >= unitMs) {
      return relativeFormatter.format(Math.round(diffMs / unitMs), unit)
    }
  }
  return 'just now'
}
