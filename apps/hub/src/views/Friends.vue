<script setup lang="ts">
// Friends/presence page (issue #18) inside the shell (issue #148). Loading,
// live presence, and the membership poll live in useFriendsPresence — this
// view owns only the add/accept/decline/remove actions.
import { ref } from 'vue'
import {
  AvalonButton,
  AvalonCard,
  AvalonFriendRequestRow,
  AvalonFriendRow,
  AvalonForm,
  AvalonTextField,
} from '@avalon/ui'
import * as api from '../api/client'
import { useFriendsPresence } from '../composables/useFriendsPresence'
import { useSessionStore } from '../stores/session'
import styles from './page.module.scss'

const session = useSessionStore()
const {
  loading,
  error,
  onlineFriends,
  offlineFriends,
  incomingRequests,
  outgoingRequests,
  refresh,
} = useFriendsPresence()

// Button-first: the add-friend input only appears once the player says
// they want to add someone — no open entry sits on the page by default.
const showAddFriend = ref(false)
const addFriendId = ref('')
const addingFriend = ref(false)
const addFriendError = ref('')

function cancelAddFriend() {
  showAddFriend.value = false
  addFriendId.value = ''
  addFriendError.value = ''
}

// Accepts either a raw identity id or a `display_name#1234` handle (#128) —
// a handle (anything containing '#') is resolved to an identity id first,
// since createFriendRequest always targets an identity id on the wire.
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
    cancelAddFriend()
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
  <div v-if="!loading" :class="styles.page">
    <header :class="styles.pageHeader">
      <h1 :class="styles.title">Friends</h1>
      <p :class="styles.subtitle">Who's online, who's waiting, and who you'd like to add.</p>
    </header>
    <p v-if="error" :class="styles.error">{{ error }}</p>

    <div :class="styles.grid">
      <div :class="styles.mainColumn">
        <AvalonCard :title="`Online (${onlineFriends.length})`">
          <p v-if="onlineFriends.length === 0" :class="styles.empty">No friends online right now.</p>
          <AvalonFriendRow
            v-for="friend in onlineFriends"
            :key="friend.identityId"
            :identity-id="friend.identityId"
            :display-name="friend.displayName"
            :status="friend.status"
            @remove="onRemoveFriend(friend.identityId)"
          />
        </AvalonCard>

        <AvalonCard :title="`Offline (${offlineFriends.length})`">
          <p v-if="offlineFriends.length === 0" :class="styles.empty">No offline friends.</p>
          <AvalonFriendRow
            v-for="friend in offlineFriends"
            :key="friend.identityId"
            :identity-id="friend.identityId"
            :display-name="friend.displayName"
            :status="friend.status"
            @remove="onRemoveFriend(friend.identityId)"
          />
        </AvalonCard>
      </div>

      <div :class="styles.sideColumn">
        <AvalonCard title="Add a friend" subtitle="By handle (e.g. alice#4821) or identity id.">
          <AvalonButton
            v-show="!showAddFriend"
            label="Add a friend"
            variant="primary"
            @click="showAddFriend = true"
          />
          <div v-show="showAddFriend">
            <AvalonForm
              submit-label="Send request"
              :submitting="addingFriend"
              :error="addFriendError"
              @submit="onAddFriend"
            >
              <AvalonTextField v-model="addFriendId" label="Handle or identity id" placeholder="alice#4821" />
            </AvalonForm>
            <AvalonButton label="Cancel" variant="secondary" @click="cancelAddFriend" />
          </div>
        </AvalonCard>

        <AvalonCard
          v-if="incomingRequests.length > 0 || outgoingRequests.length > 0"
          :title="`Pending requests (${incomingRequests.length + outgoingRequests.length})`"
        >
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
        </AvalonCard>
      </div>
    </div>
  </div>
</template>
