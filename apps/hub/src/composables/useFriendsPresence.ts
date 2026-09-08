// Friends list + live presence, shared by the Friends page and the Home
// dashboard (issue #148) so neither duplicates the load/subscribe/poll
// dance. Registers its own mount/unmount hooks — call it from `<script
// setup>` of whichever page needs it; only one page is mounted at a time,
// so only one presence socket is ever open.
import { computed, onMounted, onUnmounted, ref } from 'vue'
import * as api from '../api/client'
import type { PresenceSocket } from '../api/client'
import { listFriendsWithPresence, splitFriendRequests } from '../api/friends'
import type { Friend, FriendRequestView } from '../api/friends'
import type { PresenceResponse } from '../api/types'
import { useSessionStore } from '../stores/session'

// Presence itself is live via the websocket — this poll only catches
// friend-*list* membership changes (a request accepted/declined/withdrawn,
// a friend removed), which aren't pushed.
const POLL_INTERVAL_MS = 5 * 60_000

export function useFriendsPresence() {
  const session = useSessionStore()

  const selfId = ref('')
  const friends = ref<Friend[]>([])
  const requests = ref<FriendRequestView[]>([])
  const loading = ref(true)
  const error = ref('')

  const onlineFriends = computed(() => friends.value.filter((f) => f.status !== 'Offline'))
  const offlineFriends = computed(() => friends.value.filter((f) => f.status === 'Offline'))
  const incomingRequests = computed(() => requests.value.filter((r) => r.direction === 'incoming'))
  const outgoingRequests = computed(() => requests.value.filter((r) => r.direction === 'outgoing'))

  let pollHandle: ReturnType<typeof setInterval> | undefined
  let presenceSocket: PresenceSocket | undefined

  // A push for an id not currently in `friends` (e.g. arriving just after
  // that friend was removed) is a no-op — there's no row to update.
  function onPresenceUpdate(presence: PresenceResponse) {
    const friend = friends.value.find((f) => f.identityId === presence.identity_id)
    if (friend) friend.status = presence.status
  }

  async function refresh() {
    if (!session.token) return
    try {
      const [friendList, requestList] = await Promise.all([
        listFriendsWithPresence(session.token, selfId.value),
        api.listFriendRequests(session.token),
      ])
      friends.value = friendList
      requests.value = splitFriendRequests(requestList, selfId.value)
      presenceSocket?.subscribe(friendList.map((f) => f.identityId))
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    }
  }

  onMounted(async () => {
    if (!session.token) return
    try {
      const profile = await api.getMe(session.token)
      selfId.value = profile.identity_id
      presenceSocket = api.openPresenceSocket(session.token, onPresenceUpdate)
      await refresh()
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    } finally {
      loading.value = false
    }
    pollHandle = setInterval(refresh, POLL_INTERVAL_MS)
  })

  onUnmounted(() => {
    if (pollHandle) clearInterval(pollHandle)
    presenceSocket?.close()
  })

  return {
    selfId,
    friends,
    requests,
    loading,
    error,
    onlineFriends,
    offlineFriends,
    incomingRequests,
    outgoingRequests,
    refresh,
  }
}
