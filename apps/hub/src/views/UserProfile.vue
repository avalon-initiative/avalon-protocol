<script setup lang="ts">
// Issue #393: a read-only profile card for another identity — reachable
// from a friend row or a guild member row, neither of which had anywhere
// to link to before this. Issue #403 widened it to the full
// self-description fields (bio/pronouns/links/etc.) via a dedicated
// single-identity endpoint, GET /identities/:id/profile — same exposure
// level as that identity's own GET /me, not the narrower batch
// GET /identities/profiles shape used elsewhere for roster resolution.
// Issue #460 added an actions row (add/remove friend, block/unblock) and
// rendering of effective_main_guild/banner_url, which this card already
// fetched but never showed.
import { computed, onMounted, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { AvalonAvatar, AvalonButton, AvalonCard, AvalonPresenceBadge } from '@avalon/ui'
import { AvalonApiError } from '../api/errors'
import * as api from '../api/client'
import type {
  FriendRequestResponse,
  PresenceStatus,
  PublicIdentityProfileResponse,
} from '../api/types'
import { useSessionStore } from '../stores/session'
import page from '../styles/page.module.scss'
import styles from '../styles/UserProfile.module.scss'

const route = useRoute()
const router = useRouter()
const session = useSessionStore()

const identityId = computed(() => route.params.id as string)
const profile = ref<PublicIdentityProfileResponse | null>(null)
const status = ref<PresenceStatus>('Offline')
const loading = ref(true)
const error = ref('')

const selfId = ref('')
const isFriend = ref(false)
const incomingRequest = ref<FriendRequestResponse | null>(null)
const outgoingRequest = ref<FriendRequestResponse | null>(null)
const isBlocked = ref(false)
const mainGuildName = ref('')
const actionError = ref('')
const actionPending = ref(false)

// Never true for the caller's own identity id — Add friend/Remove
// friend/Block only ever make sense against someone else.
const isSelf = computed(() => selfId.value !== '' && selfId.value === identityId.value)

async function loadRelationship() {
  if (!session.token || isSelf.value) return
  const [friendships, requests, blocks] = await Promise.all([
    api.listFriends(session.token),
    api.listFriendRequests(session.token),
    api.listBlocks(session.token),
  ])
  isFriend.value = friendships.some((f) => f.a === identityId.value || f.b === identityId.value)
  incomingRequest.value =
    requests.find((r) => r.from === identityId.value && r.to === selfId.value) ?? null
  outgoingRequest.value =
    requests.find((r) => r.from === selfId.value && r.to === identityId.value) ?? null
  isBlocked.value = blocks.some((b) => b.blocked === identityId.value)
}

async function load() {
  if (!session.token) return
  loading.value = true
  error.value = ''
  try {
    const [fetchedProfile, presences, me] = await Promise.all([
      api.getIdentityProfile(session.token, identityId.value),
      api.getPresence(session.token, [identityId.value]),
      api.getMe(session.token),
    ])
    profile.value = fetchedProfile
    status.value = presences[0]?.status ?? 'Offline'
    selfId.value = me.identity_id
    if (fetchedProfile.effective_main_guild) {
      try {
        const guild = await api.getGuild(session.token, fetchedProfile.effective_main_guild)
        mainGuildName.value = guild.name
      } catch {
        // The linked guild may have been deleted, or no longer readable —
        // not worth failing the whole profile load over a secondary field.
        mainGuildName.value = ''
      }
    }
    await loadRelationship()
  } catch (e) {
    if (e instanceof AvalonApiError && e.status === 404) {
      profile.value = null
    } else {
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    }
  } finally {
    loading.value = false
  }
}

onMounted(load)

async function onAddFriend() {
  if (!session.token) return
  actionError.value = ''
  actionPending.value = true
  try {
    await api.createFriendRequest(session.token, { to: identityId.value })
    await loadRelationship()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    actionPending.value = false
  }
}

async function onRemoveFriend() {
  if (!session.token) return
  actionError.value = ''
  actionPending.value = true
  try {
    await api.removeFriend(session.token, identityId.value)
    await loadRelationship()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    actionPending.value = false
  }
}

async function onAcceptRequest() {
  if (!session.token || !incomingRequest.value) return
  actionError.value = ''
  actionPending.value = true
  try {
    await api.acceptFriendRequest(session.token, incomingRequest.value.id)
    await loadRelationship()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    actionPending.value = false
  }
}

async function onWithdrawOrDeclineRequest(requestId: string) {
  if (!session.token) return
  actionError.value = ''
  actionPending.value = true
  try {
    await api.declineOrWithdrawFriendRequest(session.token, requestId)
    await loadRelationship()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    actionPending.value = false
  }
}

// Issue #97: the blocked party is never told, through any client-visible
// signal — this action's own success/failure feedback is only ever shown
// to the caller who clicked it, never surfaced to identityId in any form.
// Blocking also auto-withdraws any pending friend request between the two
// (server-side, crates/server/src/blocks.rs::create_block) but does NOT
// remove an existing friendship — that stays until removed separately.
async function onBlock() {
  if (!session.token) return
  actionError.value = ''
  actionPending.value = true
  try {
    await api.createBlock(session.token, { identity_id: identityId.value })
    await loadRelationship()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    actionPending.value = false
  }
}

async function onUnblock() {
  if (!session.token) return
  actionError.value = ''
  actionPending.value = true
  try {
    await api.removeBlock(session.token, identityId.value)
    await loadRelationship()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    actionPending.value = false
  }
}
</script>

<template>
  <div :class="page.page">
    <button :class="styles.back" type="button" @click="router.back()">← Back</button>

    <p v-if="loading" :class="page.empty">Loading…</p>
    <p v-else-if="error" :class="page.error">{{ error }}</p>

    <AvalonCard v-else-if="profile">
      <img v-if="profile.banner_url" :src="profile.banner_url" alt="" :class="styles.banner" />
      <div :class="styles.header">
        <AvalonAvatar :src="profile.avatar_url" :name="profile.display_name" size="xl" />
        <div :class="styles.identity">
          <h1 :class="page.title">{{ profile.display_name }}</h1>
          <p :class="page.subtitle">{{ profile.handle }}</p>
          <p v-if="profile.pronouns" :class="styles.pronouns">{{ profile.pronouns }}</p>
        </div>
        <AvalonPresenceBadge :status="status" />
      </div>

      <p v-if="!isSelf" :class="styles.actions">
        <AvalonButton
          v-if="!isFriend && !incomingRequest && !outgoingRequest"
          label="Add friend"
          variant="primary"
          :disabled="actionPending"
          @click="onAddFriend"
        />
        <template v-if="incomingRequest">
          <AvalonButton label="Accept friend request" variant="primary" :disabled="actionPending" @click="onAcceptRequest" />
          <AvalonButton
            label="Decline"
            variant="secondary"
            :disabled="actionPending"
            @click="onWithdrawOrDeclineRequest(incomingRequest.id)"
          />
        </template>
        <AvalonButton
          v-if="outgoingRequest"
          label="Cancel request"
          variant="secondary"
          :disabled="actionPending"
          @click="onWithdrawOrDeclineRequest(outgoingRequest.id)"
        />
        <AvalonButton
          v-if="isFriend"
          label="Remove friend"
          variant="secondary"
          :disabled="actionPending"
          @click="onRemoveFriend"
        />
        <AvalonButton
          v-if="!isBlocked"
          label="Block"
          variant="danger"
          :disabled="actionPending"
          @click="onBlock"
        />
        <AvalonButton
          v-else
          label="Unblock"
          variant="secondary"
          :disabled="actionPending"
          @click="onUnblock"
        />
      </p>
      <p v-if="actionError" :class="page.error">{{ actionError }}</p>

      <p v-if="profile.status" :class="styles.status">{{ profile.status }}</p>
      <p v-if="profile.bio" :class="styles.bio">{{ profile.bio }}</p>

      <ul v-if="profile.location || profile.favorite_genres.length" :class="styles.meta">
        <li v-if="profile.location">📍 {{ profile.location }}</li>
        <li v-if="profile.favorite_genres.length">{{ profile.favorite_genres.join(', ') }}</li>
      </ul>

      <p v-if="profile.effective_main_guild" :class="styles.mainGuild">
        Main guild:
        <RouterLink :to="{ name: 'guild', params: { id: profile.effective_main_guild } }">
          {{ mainGuildName || profile.effective_main_guild }}
        </RouterLink>
      </p>

      <ul v-if="profile.links.length" :class="styles.links">
        <li v-for="link in profile.links" :key="link">
          <a :href="link" target="_blank" rel="noopener noreferrer">{{ link }}</a>
        </li>
      </ul>

      <p :class="styles.note">Presence only shows if this user has made it visible to you.</p>
    </AvalonCard>

    <p v-else :class="page.empty">That user couldn't be found.</p>
  </div>
</template>
