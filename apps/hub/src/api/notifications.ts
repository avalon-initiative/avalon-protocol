// Pure logic backing useNotificationSummary — the Hub-wide
// pending-action badge. Two of the seven aggregated sources need actual
// "have I seen this" state that doesn't exist anywhere server-side yet
// (direct messages have no unread tracking at all; being named a
// recovery guardian, #443, has no "acknowledged" concept either) — both
// tracked here the same client-local way guildAnnouncements.ts already
// tracks announcement read state: a last-seen marker in localStorage,
// never a server round trip.
//
// The other five sources (incoming friend requests, guild join requests
// awaiting review, guild invites received, device-grant approvals,
// guardian-approval requests) are genuinely actionable pending state —
// their count comes straight from "how many are currently outstanding,"
// same as the badge on e.g. a mail client's unactioned-request folder.
// That count only drops by resolving the item (accepting/declining/
// approving/rejecting), not merely by visiting the page — unlike the two
// "have I seen this yet" sources above, resolving is the correct signal
// for something the caller must actually act on.
import type { ConversationMessage } from '@avalon/sdk'

const DM_LAST_SEEN_STORAGE_KEY = 'avalon:conversations:lastSeen'
const GUARDIAN_OF_SEEN_STORAGE_KEY = 'avalon:guardianOf:seen'

type LastSeenByConversation = Record<string, string>

// Same defensive posture guildAnnouncements.ts's loadLastSeen documents:
// localStorage can throw or be unavailable, never let that break the
// feature — just fall back to "nothing marked seen yet".
export function loadConversationsLastSeen(): LastSeenByConversation {
  try {
    const raw = localStorage.getItem(DM_LAST_SEEN_STORAGE_KEY)
    if (!raw) return {}
    const parsed = JSON.parse(raw)
    return parsed && typeof parsed === 'object' ? parsed : {}
  } catch {
    return {}
  }
}

function saveConversationsLastSeen(lastSeen: LastSeenByConversation) {
  try {
    localStorage.setItem(DM_LAST_SEEN_STORAGE_KEY, JSON.stringify(lastSeen))
  } catch {
    // Best-effort only — see module doc comment.
  }
}

// Marks `conversationId` seen as of `at` (the newest message's own
// `sent_at`) — mirrors guildAnnouncements.ts's markChannelSeen exactly:
// only needs the newest timestamp recorded, not a per-message receipt.
export function markConversationSeen(conversationId: string, at: string) {
  const lastSeen = loadConversationsLastSeen()
  const current = lastSeen[conversationId]
  if (current && new Date(current).getTime() >= new Date(at).getTime()) return
  lastSeen[conversationId] = at
  saveConversationsLastSeen(lastSeen)
}

// Pure, testable without localStorage — mirrors guildAnnouncements.ts's
// isUnread. `lastMessage` is null for a conversation with no messages at
// all yet (never unread).
export function isConversationUnread(
  conversationId: string,
  lastMessage: ConversationMessage | null,
  selfId: string,
  lastSeen: LastSeenByConversation,
): boolean {
  if (!lastMessage || lastMessage.author === selfId) return false
  const seenAt = lastSeen[conversationId]
  if (!seenAt) return true
  return new Date(lastMessage.sentAt).getTime() > new Date(seenAt).getTime()
}

type GuardianOfSeen = string[]

export function loadGuardianOfSeen(): GuardianOfSeen {
  try {
    const raw = localStorage.getItem(GUARDIAN_OF_SEEN_STORAGE_KEY)
    if (!raw) return []
    const parsed = JSON.parse(raw)
    return Array.isArray(parsed) ? parsed : []
  } catch {
    return []
  }
}

// Marks every currently-listed guardian-of identity as seen — called when
// Profile.vue's "named a guardian" section actually renders them, same
// "visiting the real feature is what clears it" idiom
// onSelectAnnouncement's markChannelSeen already establishes.
export function markGuardianOfSeen(identityIds: string[]) {
  try {
    const seen = new Set([...loadGuardianOfSeen(), ...identityIds])
    localStorage.setItem(GUARDIAN_OF_SEEN_STORAGE_KEY, JSON.stringify([...seen]))
  } catch {
    // Best-effort only.
  }
}

// Pure, testable without localStorage.
export function countNewGuardianOf(identityIds: string[], seen: GuardianOfSeen): number {
  const seenSet = new Set(seen)
  return identityIds.filter((id) => !seenSet.has(id)).length
}
