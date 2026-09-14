<script setup lang="ts">
// Issue #393: a read-only profile card for another identity — reachable
// from a friend row or a guild member row, neither of which had anywhere
// to link to before this. Issue #403 widened it to the full
// self-description fields (bio/pronouns/links/etc.) via a dedicated
// single-identity endpoint, GET /identities/:id/profile — same exposure
// level as that identity's own GET /me, not the narrower batch
// GET /identities/profiles shape used elsewhere for roster resolution.
import { computed, onMounted, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { AvalonAvatar, AvalonCard, AvalonPresenceBadge } from '@avalon/ui'
import { AvalonApiError } from '../api/errors'
import { getIdentityProfile, getPresence } from '../api/client'
import type { PresenceStatus, PublicIdentityProfileResponse } from '../api/types'
import { useSessionStore } from '../stores/session'
import page from './page.module.scss'
import styles from './UserProfile.module.scss'

const route = useRoute()
const router = useRouter()
const session = useSessionStore()

const identityId = computed(() => route.params.id as string)
const profile = ref<PublicIdentityProfileResponse | null>(null)
const status = ref<PresenceStatus>('Offline')
const loading = ref(true)
const error = ref('')

async function load() {
  if (!session.token) return
  loading.value = true
  error.value = ''
  try {
    const [fetchedProfile, presences] = await Promise.all([
      getIdentityProfile(session.token, identityId.value),
      getPresence(session.token, [identityId.value]),
    ])
    profile.value = fetchedProfile
    status.value = presences[0]?.status ?? 'Offline'
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
</script>

<template>
  <div :class="page.page">
    <button :class="styles.back" type="button" @click="router.back()">← Back</button>

    <p v-if="loading" :class="page.empty">Loading…</p>
    <p v-else-if="error" :class="page.error">{{ error }}</p>

    <AvalonCard v-else-if="profile">
      <div :class="styles.header">
        <AvalonAvatar :src="profile.avatar_url" :name="profile.display_name" size="xl" />
        <div :class="styles.identity">
          <h1 :class="page.title">{{ profile.display_name }}</h1>
          <p :class="page.subtitle">{{ profile.handle }}</p>
          <p v-if="profile.pronouns" :class="styles.pronouns">{{ profile.pronouns }}</p>
        </div>
        <AvalonPresenceBadge :status="status" />
      </div>

      <p v-if="profile.status" :class="styles.status">{{ profile.status }}</p>
      <p v-if="profile.bio" :class="styles.bio">{{ profile.bio }}</p>

      <ul v-if="profile.location || profile.favorite_genres.length" :class="styles.meta">
        <li v-if="profile.location">📍 {{ profile.location }}</li>
        <li v-if="profile.favorite_genres.length">{{ profile.favorite_genres.join(', ') }}</li>
      </ul>

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
