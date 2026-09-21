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
// fetched but never showed. Issue #465 added a "Published by connected
// apps" card: GET /identities/:id/integrator-data (#384) already resolves
// everything a connected integrator has published about this identity,
// pre-filtered to visible fields, but nothing in the Hub read it.
import { computed, onMounted, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { AvalonAvatar, AvalonButton, AvalonCard, AvalonPresenceBadge } from '@avalon/ui'
import { NotFoundError } from '@avalon/sdk'
import { listPublishedIntegratorData, type PublishedIntegratorData } from '../api/integratorData'
import type { FriendRequest, PresenceStatus, PublicIdentityProfile } from '@avalon/sdk'
import { useSessionStore } from '../api/session'
import page from '../styles/page.module.scss'
import styles from '../styles/UserProfile.module.scss'

const route = useRoute()
const router = useRouter()
const session = useSessionStore()

const identityId = computed(() => route.params.id as string)
const profile = ref<PublicIdentityProfile | null>(null)
const status = ref<PresenceStatus>('Offline')
const loading = ref(true)
const error = ref('')

const selfId = ref('')
const isFriend = ref(false)
const incomingRequest = ref<FriendRequest | null>(null)
const outgoingRequest = ref<FriendRequest | null>(null)
const isBlocked = ref(false)
const mainGuildName = ref('')
const actionError = ref('')
const actionPending = ref(false)

// Issue #465. Empty when nothing is published, or nothing is visible to
// the caller specifically — the two are indistinguishable by design, same
// posture the rest of this visibility model already takes elsewhere.
const publishedData = ref<PublishedIntegratorData[]>([])
const publishedDataError = ref('')

// Never true for the caller's own identity id — Add friend/Remove
// friend/Block only ever make sense against someone else.
const isSelf = computed(() => selfId.value !== '' && selfId.value === identityId.value)

async function loadRelationship() {
  const s = session.session
  if (!s || isSelf.value) return
  const [friendships, requests, blocks] = await Promise.all([s.friends(), s.friendRequests(), s.blocks()])
  isFriend.value =
    Array.isArray(friendships) &&
    friendships.some((f) => f.a === identityId.value || f.b === identityId.value)
  const requestList = Array.isArray(requests) ? requests : []
  incomingRequest.value =
    requestList.find((r) => r.from === identityId.value && r.to === selfId.value) ?? null
  outgoingRequest.value =
    requestList.find((r) => r.from === selfId.value && r.to === identityId.value) ?? null
  isBlocked.value = Array.isArray(blocks) && blocks.some((b) => b.blocked === identityId.value)
}

async function load() {
  const s = session.session
  if (!s) return
  loading.value = true
  error.value = ''
  try {
    const [fetchedProfile, presences] = await Promise.all([
      s.identityProfile(identityId.value),
      s.presenceOf([identityId.value]),
    ])
    profile.value = fetchedProfile
    status.value = (Array.isArray(presences) && presences[0]?.status) || 'Offline'
    selfId.value = s.identity().id
    if (fetchedProfile.effectiveMainGuild) {
      try {
        const guild = await s.getGuild(fetchedProfile.effectiveMainGuild)
        mainGuildName.value = guild.name
      } catch {
        // The linked guild may have been deleted, or no longer readable —
        // not worth failing the whole profile load over a secondary field.
        mainGuildName.value = ''
      }
    }
    await loadRelationship()
    try {
      publishedData.value = await listPublishedIntegratorData(identityId.value)
    } catch (e) {
      // A secondary card — not worth failing the whole profile load over.
      publishedDataError.value = e instanceof Error ? e.message : 'Something went wrong.'
    }
  } catch (e) {
    if (e instanceof NotFoundError) {
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
  const s = session.session
  if (!s) return
  actionError.value = ''
  actionPending.value = true
  try {
    await s.createFriendRequest(identityId.value)
    await loadRelationship()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    actionPending.value = false
  }
}

async function onRemoveFriend() {
  const s = session.session
  if (!s) return
  actionError.value = ''
  actionPending.value = true
  try {
    await s.removeFriend(identityId.value)
    await loadRelationship()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    actionPending.value = false
  }
}

async function onAcceptRequest() {
  const s = session.session
  if (!s || !incomingRequest.value) return
  actionError.value = ''
  actionPending.value = true
  try {
    await s.acceptFriendRequest(incomingRequest.value.id)
    await loadRelationship()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    actionPending.value = false
  }
}

async function onWithdrawOrDeclineRequest(requestId: string) {
  const s = session.session
  if (!s) return
  actionError.value = ''
  actionPending.value = true
  try {
    await s.declineOrWithdrawFriendRequest(requestId)
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
  const s = session.session
  if (!s) return
  actionError.value = ''
  actionPending.value = true
  try {
    await s.block(identityId.value)
    await loadRelationship()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    actionPending.value = false
  }
}

async function onUnblock() {
  const s = session.session
  if (!s) return
  actionError.value = ''
  actionPending.value = true
  try {
    await s.unblock(identityId.value)
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
      <img v-if="profile.bannerUrl" :src="profile.bannerUrl" alt="" :class="styles.banner" />
      <div :class="styles.header">
        <AvalonAvatar :src="profile.avatarUrl" :name="profile.displayName" size="xl" />
        <div :class="styles.identity">
          <h1 :class="page.title">{{ profile.displayName }}</h1>
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

      <ul v-if="profile.location || profile.favoriteGenres.length" :class="styles.meta">
        <li v-if="profile.location">📍 {{ profile.location }}</li>
        <li v-if="profile.favoriteGenres.length">{{ profile.favoriteGenres.join(', ') }}</li>
      </ul>

      <p v-if="profile.effectiveMainGuild" :class="styles.mainGuild">
        Main guild:
        <RouterLink :to="{ name: 'guild', params: { id: profile.effectiveMainGuild } }">
          {{ mainGuildName || profile.effectiveMainGuild }}
        </RouterLink>
      </p>

      <ul v-if="profile.links.length" :class="styles.links">
        <li v-for="link in profile.links" :key="link">
          <a :href="link" target="_blank" rel="noopener noreferrer">{{ link }}</a>
        </li>
      </ul>

      <p :class="styles.note">Presence only shows if this user has made it visible to you.</p>
    </AvalonCard>

    <AvalonCard
      v-if="profile"
      title="Published by connected apps"
      subtitle="Whatever a connected game, app, or service has chosen to make visible about this player."
    >
      <p v-if="publishedDataError" :class="page.error">{{ publishedDataError }}</p>
      <p v-else-if="publishedData.length === 0" :class="page.empty">Nothing published here yet.</p>
      <div v-for="entry in publishedData" :key="entry.schema" :class="styles.publishedEntry">
        <h3 :class="styles.subheading">{{ entry.integratorName ?? entry.integratorSlug }}</h3>
        <ul :class="styles.publishedFields">
          <li v-for="(value, field) in entry.fields" :key="field">
            {{ field }}: {{ value }}
          </li>
        </ul>
      </div>
    </AvalonCard>

    <p v-else :class="page.empty">That user couldn't be found.</p>
  </div>
</template>
