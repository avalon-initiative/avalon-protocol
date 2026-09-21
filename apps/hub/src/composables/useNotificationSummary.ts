// Issue #466 — the Hub-wide pending-action badge (HubShell.vue). Every
// source below already has a real, working page; this only aggregates
// their existing data into one combined count, reusing each source's own
// endpoint rather than inventing a new aggregate server-side. See
// api/notifications.ts's module doc comment for which sources are
// genuinely-actionable-pending-state (cleared by resolving the item) vs.
// "have I seen this yet" (cleared by visiting, client-local state).
import { computed, onMounted, onUnmounted, ref } from 'vue'
import {
  countNewGuardianOf,
  isConversationUnread,
  loadConversationsLastSeen,
  loadGuardianOfSeen,
} from '../api/notifications'
import { useSessionStore } from '../api/session'

// Deliberately not as tight as Profile.vue's own in-page 5s poll (issue
// #201/#307's approval flows) — this is an ambient, always-mounted
// summary, not a page the caller is actively watching for a live
// approval. Slower than every source's own dedicated page poll, so it
// never adds a new *faster* cadence anywhere (this composable's own
// invariant).
const POLL_INTERVAL_MS = 30_000

export function useNotificationSummary() {
  const session = useSessionStore()

  const selfId = ref('')
  const incomingFriendRequestCount = ref(0)
  const guildJoinRequestCount = ref(0)
  const guildInviteCount = ref(0)
  const deviceGrantCount = ref(0)
  const guardianRequestCount = ref(0)
  const newGuardianOfCount = ref(0)
  const unreadDmCount = ref(0)
  const loading = ref(true)
  const error = ref('')

  const totalCount = computed(
    () =>
      incomingFriendRequestCount.value +
      guildJoinRequestCount.value +
      guildInviteCount.value +
      deviceGrantCount.value +
      guardianRequestCount.value +
      newGuardianOfCount.value +
      unreadDmCount.value,
  )

  let pollHandle: ReturnType<typeof setInterval> | undefined

  async function refresh() {
    const s = session.session
    if (!s) return
    try {
      if (!selfId.value) {
        selfId.value = s.identity().id
      }

      const [friendRequests, memberships, invites, grants, guardianRequests, guardianOfList, conversations] =
        await Promise.all([
          s.friendRequests().catch(() => []),
          s.myGuilds().catch(() => []),
          s.myGuildInvites().catch(() => []),
          s.listDeviceGrants('pending').catch(() => []),
          s.guardianRequests().catch(() => []),
          s.guardianOf().catch(() => []),
          s.listConversations().catch(() => []),
        ])

      incomingFriendRequestCount.value = Array.isArray(friendRequests)
        ? friendRequests.filter((r) => r.to === selfId.value).length
        : 0

      // manage_members-gated per guild — a plain member 403s, which just
      // means this guild contributes 0, not an error worth surfacing.
      const guildIds = Array.isArray(memberships) ? memberships.map((m) => m.guildId) : []
      const joinRequestCounts = await Promise.all(
        guildIds.map((guildId) =>
          s
            .listJoinRequests(guildId)
            .then((rows) => (Array.isArray(rows) ? rows.length : 0))
            .catch(() => 0),
        ),
      )
      guildJoinRequestCount.value = joinRequestCounts.reduce((sum, n) => sum + n, 0)

      guildInviteCount.value = Array.isArray(invites) ? invites.length : 0
      deviceGrantCount.value = Array.isArray(grants) ? grants.length : 0
      guardianRequestCount.value = Array.isArray(guardianRequests) ? guardianRequests.length : 0

      const guardianOfIds = Array.isArray(guardianOfList) ? guardianOfList.map((g) => g.identityId) : []
      newGuardianOfCount.value = countNewGuardianOf(guardianOfIds, loadGuardianOfSeen())

      const conversationList = Array.isArray(conversations) ? conversations : []
      const lastMessages = await Promise.all(
        conversationList.map((c) =>
          s
            .conversationMessages(c.id, undefined, 1)
            .then((msgs) => (Array.isArray(msgs) ? (msgs[0] ?? null) : null))
            .catch(() => null),
        ),
      )
      const lastSeen = loadConversationsLastSeen()
      unreadDmCount.value = conversationList.filter((c, i) =>
        isConversationUnread(c.id, lastMessages[i], selfId.value, lastSeen),
      ).length
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    } finally {
      loading.value = false
    }
  }

  onMounted(async () => {
    await refresh()
    pollHandle = setInterval(refresh, POLL_INTERVAL_MS)
  })

  onUnmounted(() => {
    if (pollHandle) clearInterval(pollHandle)
  })

  return {
    totalCount,
    incomingFriendRequestCount,
    guildJoinRequestCount,
    guildInviteCount,
    deviceGrantCount,
    guardianRequestCount,
    newGuardianOfCount,
    unreadDmCount,
    loading,
    error,
    refresh,
  }
}
