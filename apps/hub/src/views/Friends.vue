<script setup lang="ts">
// Friends/presence tab (issue #18) inside the shell (issue #130). No more
// AvalonAuthCard wrapper or manual "back to profile" link — the shell's
// tab nav replaces both.
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { AvalonFriendRequestRow, AvalonFriendRow, AvalonForm, AvalonTextField } from '@avalon/ui'
import * as api from '../api/client'
import { listFriendsWithPresence, splitFriendRequests } from '../api/friends'
import type { Friend, FriendRequestView } from '../api/friends'
import { useSessionStore } from '../stores/session'

// There's no way for the client to know the server's configured
// AVALON_PRESENCE_TTL_SECS, so this doesn't try to sync with it exactly —
// just a reasonable liveness interval for milestone 1. A push-based
// subscription (#119) is the real fix, not a blocker here.
const POLL_INTERVAL_MS = 60_000

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

async function refresh() {
  if (!session.token) return
  try {
    const [friendList, requestList] = await Promise.all([
      listFriendsWithPresence(session.token, selfId.value),
      api.listFriendRequests(session.token),
    ])
    friends.value = friendList
    requests.value = splitFriendRequests(requestList, selfId.value)
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}

onMounted(async () => {
  if (!session.token) return
  try {
    const profile = await api.getMe(session.token)
    selfId.value = profile.identity_id
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
})

async function onAddFriend() {
  if (!session.token) return
  addFriendError.value = ''
  addingFriend.value = true
  try {
    await api.createFriendRequest(session.token, { to: addFriendId.value })
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
      <AvalonTextField v-model="addFriendId" label="Identity id" placeholder="Their identity id" />
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
