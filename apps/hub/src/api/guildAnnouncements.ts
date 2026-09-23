// Issue #280: guild announcement alerts — a member sees a visible alert
// when their guild's announcement-only channel gets a new post, without
// manually checking each guild. GET /me/guild-announcements (api/client.ts)
// is a plain read with no server-side unread/seen tracking (matching
// #22/#74/#253's "chat is operational-tier, not protocol history" — see
// crates/server/src/guild_messages.rs's own doc comment on the endpoint):
// read/unread is entirely this Hub's own client-local concern, computed
// from a per-channel "last seen" timestamp kept in localStorage.
import type { GuildAnnouncementAlert } from '@avalon-initiative/protocol-sdk'

const LAST_SEEN_STORAGE_KEY = 'avalon:guildAnnouncements:lastSeen'

type LastSeenByChannel = Record<string, string>

// localStorage can throw (private browsing, cleared/blocked site data) or
// simply be unavailable — never let a read/write here break the alerts
// feature, just fall back to "nothing marked seen yet".
export function loadLastSeen(): LastSeenByChannel {
  try {
    const raw = localStorage.getItem(LAST_SEEN_STORAGE_KEY)
    if (!raw) return {}
    const parsed = JSON.parse(raw)
    return parsed && typeof parsed === 'object' ? parsed : {}
  } catch {
    return {}
  }
}

function saveLastSeen(lastSeen: LastSeenByChannel) {
  try {
    localStorage.setItem(LAST_SEEN_STORAGE_KEY, JSON.stringify(lastSeen))
  } catch {
    // Best-effort only — see module doc comment.
  }
}

// Marks `channelId` seen as of `at` (an alert's own `sent_at`, not
// "now") — so a channel with several unseen posts only needs the newest
// one's timestamp recorded, not a full read receipt per message.
export function markChannelSeen(channelId: string, at: string) {
  const lastSeen = loadLastSeen()
  const current = lastSeen[channelId]
  if (current && new Date(current).getTime() >= new Date(at).getTime()) return
  lastSeen[channelId] = at
  saveLastSeen(lastSeen)
}

// Pure, testable without localStorage: an alert is unread if its channel
// has never been seen, or was last seen strictly before this alert was
// sent.
export function isUnread(alert: GuildAnnouncementAlert, lastSeen: LastSeenByChannel): boolean {
  const seenAt = lastSeen[alert.channelId]
  if (!seenAt) return true
  return new Date(alert.sentAt).getTime() > new Date(seenAt).getTime()
}

export function countUnread(
  alerts: GuildAnnouncementAlert[],
  lastSeen: LastSeenByChannel,
): number {
  return alerts.filter((alert) => isUnread(alert, lastSeen)).length
}

// A short, human body preview for the alerts panel — same "truncate with
// an ellipsis past a character cap" convention as other list surfaces in
// this Hub (e.g. sortAchievements' name fallbacks), not a full-message
// render.
const PREVIEW_MAX_CHARS = 80

export function previewBody(body: string): string {
  const trimmed = body.trim()
  return trimmed.length > PREVIEW_MAX_CHARS
    ? `${trimmed.slice(0, PREVIEW_MAX_CHARS - 1)}…`
    : trimmed
}
