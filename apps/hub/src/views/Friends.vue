<script setup lang="ts">
// Friends/presence page (issue #18) inside the shell (issue #148). Loading,
// live presence, and the membership poll live in useFriendsPresence — this
// view owns only the add/accept/decline/remove actions, plus the
// "people you may know" suggestions section (issue #204).
import { onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import {
  AvalonButton,
  AvalonCard,
  AvalonFriendRequestRow,
  AvalonFriendRow,
  AvalonForm,
  AvalonSuggestionRow,
  AvalonTextField,
} from '@avalon/ui'
import * as api from '../api/client'
import { listSuggestions } from '../api/discovery'
import type { Suggestion } from '../api/discovery'
import type { SearchResultIdentity } from '../api/types'
import { useFriendsPresence } from '../composables/useFriendsPresence'
import { useSessionStore } from '../stores/session'
import styles from './page.module.scss'

const session = useSessionStore()
const router = useRouter()
const {
  loading,
  error,
  onlineFriends,
  offlineFriends,
  incomingRequests,
  outgoingRequests,
  refresh,
} = useFriendsPresence()

// "People you may know" (issue #204) — friends-of-friends and mutual-guild
// suggestions, loaded once on mount alongside the rest of the page. A
// suggestion never disappears the moment it's added (the server response
// doesn't change until a page reload) — instead its row flips to a
// disabled "Requested" state, tracked locally here.
const suggestions = ref<Suggestion[]>([])
const requestedSuggestionIds = ref<Set<string>>(new Set())

async function loadSuggestions() {
  if (!session.token) return
  try {
    suggestions.value = await listSuggestions(session.token)
  } catch {
    // Suggestions are a secondary surface — a failure here shouldn't block
    // or clutter the primary friends/requests error state above.
    suggestions.value = []
  }
}

async function onAddSuggestion(identityId: string) {
  if (!session.token) return
  try {
    await api.createFriendRequest(session.token, { to: identityId })
    requestedSuggestionIds.value = new Set(requestedSuggestionIds.value).add(identityId)
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}

onMounted(loadSuggestions)

// Player search (issue #205) — the opt-in global counterpart to "people
// you may know" above. Only ever returns identities that have turned on
// their own `discoverable` preference (Profile.vue); a blank query issues
// no request at all (see api.searchIdentities). Reuses AvalonSuggestionRow
// the same way suggestions does, including the same "flip to Requested
// rather than disappear" posture for a result someone's just sent a
// request to.
const searchQuery = ref('')
const searchResults = ref<SearchResultIdentity[]>([])
const searching = ref(false)
const searchError = ref('')
const requestedSearchIds = ref<Set<string>>(new Set())
const hasSearched = ref(false)

async function onSearch() {
  if (!session.token) return
  searchError.value = ''
  searching.value = true
  hasSearched.value = true
  try {
    const response = await api.searchIdentities(session.token, searchQuery.value)
    searchResults.value = response.results
  } catch (e) {
    searchError.value = e instanceof Error ? e.message : 'Something went wrong.'
    searchResults.value = []
  } finally {
    searching.value = false
  }
}

async function onAddFromSearch(identityId: string) {
  if (!session.token) return
  try {
    await api.createFriendRequest(session.token, { to: identityId })
    requestedSearchIds.value = new Set(requestedSearchIds.value).add(identityId)
  } catch (e) {
    searchError.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}

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

// Issue #393: opens a friend's read-only profile card.
function onViewProfile(identityId: string) {
  router.push({ name: 'player-profile', params: { id: identityId } })
}

// Starts (or opens the existing) conversation with this friend and jumps
// straight to it — POST /conversations is idempotent on the participant
// set, so this is never a duplicate even if one already exists.
async function onMessageFriend(identityId: string) {
  if (!session.token) return
  try {
    const conversation = await api.createConversation(session.token, {
      participants: [identityId],
    })
    router.push({ name: 'conversation', params: { id: conversation.id } })
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
            @message="onMessageFriend(friend.identityId)"
            @remove="onRemoveFriend(friend.identityId)"
            @view="onViewProfile(friend.identityId)"
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
            @message="onMessageFriend(friend.identityId)"
            @remove="onRemoveFriend(friend.identityId)"
            @view="onViewProfile(friend.identityId)"
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
              <template #secondary-actions>
                <AvalonButton label="Cancel" variant="secondary" @click="cancelAddFriend" />
              </template>
            </AvalonForm>
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

        <AvalonCard
          title="Search for players"
          subtitle="Finds only players who've turned on public search for their own profile."
        >
          <AvalonForm
            submit-label="Search"
            :submitting="searching"
            :error="searchError"
            @submit="onSearch"
          >
            <AvalonTextField v-model="searchQuery" label="Name or handle" placeholder="alice" />
          </AvalonForm>
          <p v-if="hasSearched && !searching && searchResults.length === 0" :class="styles.empty">
            No publicly searchable players match that.
          </p>
          <AvalonSuggestionRow
            v-for="result in searchResults"
            :key="result.identity_id"
            :identity-id="result.identity_id"
            :display-name="result.display_name"
            :avatar-url="result.avatar_url"
            :requested="requestedSearchIds.has(result.identity_id)"
            @add="onAddFromSearch(result.identity_id)"
          />
        </AvalonCard>

        <AvalonCard
          v-if="suggestions.length > 0"
          title="People you may know"
          subtitle="Friends of friends and people in your guilds."
        >
          <AvalonSuggestionRow
            v-for="suggestion in suggestions"
            :key="suggestion.identityId"
            :identity-id="suggestion.identityId"
            :display-name="suggestion.displayName"
            :requested="requestedSuggestionIds.has(suggestion.identityId)"
            @add="onAddSuggestion(suggestion.identityId)"
          />
        </AvalonCard>
      </div>
    </div>
  </div>
</template>
