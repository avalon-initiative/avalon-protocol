<script setup lang="ts">
// Friends/presence tab (issue #18) inside the shell (issue #130). No more
// AvalonAuthCard wrapper or manual "back to profile" link — the shell's
// tab nav replaces both.
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { AvalonFriendRequestRow, AvalonFriendRow, AvalonForm, AvalonTextField } from '@avalon/ui'
import * as api from '../api/client'
import type { PresenceSocket } from '../api/client'
import { listFriendsWithPresence, splitFriendRequests } from '../api/friends'
import type { Friend, FriendRequestView } from '../api/friends'
import type { PresenceResponse } from '../api/types'
import { useSessionStore } from '../stores/session'

// Presence itself is live now (issue #136's GET /ws/presence, opened
// below) — this poll only exists to catch friend-*list* membership changes
// (a request accepted/declined/withdrawn, a friend removed), which #136
// doesn't push. Slower than the old presence-liveness-driven 60s interval
// on purpose, now that presence doesn't depend on it.
const POLL_INTERVAL_MS = 5 * 60_000

const session = useSessionStore()

const selfId = ref('')
const friends = ref<Friend[]>([])
const requests = ref<FriendRequestView[]>([])
const loading = ref(true)
const error = ref('')

const addFriendId = ref('')
const addingFriend = ref(false)
const addFriendError = ref('')

const onlineFriends = computed(() => friends.value.filter((f) => f.status !== 'Offline'))
const offlineFriends = computed(() => friends.value.filter((f) => f.status === 'Offline'))
const incomingRequests = computed(() => requests.value.filter((r) => r.direction === 'incoming'))
const outgoingRequests = computed(() => requests.value.filter((r) => r.direction === 'outgoing'))

let pollHandle: ReturnType<typeof setInterval> | undefined
let presenceSocket: PresenceSocket | undefined

// Applies one pushed presence update to whichever friend row it's for.
// A push for an id not currently in `friends.value` (e.g. arriving just
// after that friend was removed) is simply a no-op — there's no row to
// update, and nothing needs cleaning up on this side.
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

// Accepts either a raw identity id (already worked pre-#128) or a
// `display_name#1234` handle — resolved to an identity id first via
// GET /friends/handle/:handle, since createFriendRequest itself always
// targets an identity id on the wire. A handle is anything containing '#';
// a raw identity id never does.
async function onAddFriend() {
  if (!session.token) return
  addFriendError.value = ''
  addingFriend.value = true
  try {
    const input = addFriendId.value.trim()
    const to = input.includes('#')
      ? (await api.resolveHandle(session.token, input)).identity_id
      : input
    await api.createFriendRequest(session.token, { to })
    addFriendId.value = ''
    await refresh()
  } catch (e) {
    addFriendError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    addingFriend.value = false
  }
}

async function onAcceptRequest(requestId: string) {
  if (!session.token) return
  try {
    await api.acceptFriendRequest(session.token, requestId)
    await refresh()
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}

async function onRemoveRequest(requestId: string) {
  if (!session.token) return
  try {
    await api.declineOrWithdrawFriendRequest(session.token, requestId)
    await refresh()
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}

async function onRemoveFriend(identityId: string) {
  if (!session.token) return
  try {
    await api.removeFriend(session.token, identityId)
    await refresh()
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}
</script>

<template>
  <section v-if="!loading">
    <p v-if="error">{{ error }}</p>

    <AvalonForm
      submit-label="Add friend"
      :submitting="addingFriend"
      :error="addFriendError"
      @submit="onAddFriend"
    >
      <AvalonTextField
        v-model="addFriendId"
        label="Handle or identity id"
        placeholder="alice#4821"
      />
    </AvalonForm>

    <section>
      <h2>Online</h2>
      <p v-if="onlineFriends.length === 0">No friends online right now.</p>
      <AvalonFriendRow
        v-for="friend in onlineFriends"
        :key="friend.identityId"
        :identity-id="friend.identityId"
        :display-name="friend.displayName"
        :status="friend.status"
        @remove="onRemoveFriend(friend.identityId)"
      />
    </section>

    <section>
      <h2>Offline</h2>
      <p v-if="offlineFriends.length === 0">No offline friends.</p>
      <AvalonFriendRow
        v-for="friend in offlineFriends"
        :key="friend.identityId"
        :identity-id="friend.identityId"
        :display-name="friend.displayName"
        :status="friend.status"
        @remove="onRemoveFriend(friend.identityId)"
      />
    </section>

    <section v-if="incomingRequests.length > 0 || outgoingRequests.length > 0">
      <h2>Pending requests</h2>
      <AvalonFriendRequestRow
        v-for="request in incomingRequests"
        :key="request.id"
        :identity-id="request.otherIdentityId"
        direction="incoming"
        @accept="onAcceptRequest(request.id)"
        @remove="onRemoveRequest(request.id)"
      />
      <AvalonFriendRequestRow
        v-for="request in outgoingRequests"
        :key="request.id"
        :identity-id="request.otherIdentityId"
        direction="outgoing"
        @remove="onRemoveRequest(request.id)"
      />
    </section>
  </section>
</template>
