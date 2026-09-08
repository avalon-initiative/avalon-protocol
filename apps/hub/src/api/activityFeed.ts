// Turns a raw HistoryEntryResponse (issue #121) into something an actual
// player would want to read — issue #146. Kept as pure functions, testable
// without mounting Activity.vue: given an event, what should the feed say.
import type { HistoryEntryResponse } from './types'

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
 * (this list will keep growing as more event kinds ship — #82) fall back to
 * the raw `kind` string rather than throwing or rendering nothing.
 */
export function summarizeActivityEntry(entry: HistoryEntryResponse): string {
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
