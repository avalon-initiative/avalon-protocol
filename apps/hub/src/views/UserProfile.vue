<script setup lang="ts">
// Issue #393: a read-only profile card for another identity — reachable
// from a friend row or a guild member row, neither of which had anywhere
// to link to before this. Deliberately shows only display name, avatar,
// handle, and live presence: `GET /identities/profiles`'s own doc comment
// (crates/server/src/handlers.rs) is explicit that bio/pronouns/etc.
// (#155/#372) are withheld from batch stranger lookup on purpose — that's
// a separate exposure-scoping decision, not something to widen here as a
// side effect (see issue #403).
import { computed, onMounted, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { AvalonAvatar, AvalonCard, AvalonPresenceBadge } from '@avalon/ui'
import { getPresence, getProfiles } from '../api/client'
import type { PresenceStatus, PublicProfileResponse } from '../api/types'
import { useSessionStore } from '../stores/session'
import page from './page.module.scss'
import styles from './UserProfile.module.scss'

const route = useRoute()
const router = useRouter()
const session = useSessionStore()

const identityId = computed(() => route.params.id as string)
const profile = ref<PublicProfileResponse | null>(null)
const status = ref<PresenceStatus>('Offline')
const loading = ref(true)
const error = ref('')

async function load() {
  if (!session.token) return
  loading.value = true
  error.value = ''
  try {
    const [profiles, presences] = await Promise.all([
      getProfiles(session.token, [identityId.value]),
      getPresence(session.token, [identityId.value]),
    ])
    profile.value = profiles[0] ?? null
    status.value = presences[0]?.status ?? 'Offline'
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    loading.value = false
  }
}

onMounted(load)

const handle = computed(() =>
  profile.value ? `${profile.value.display_name}#${profile.value.discriminator}` : '',
)
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
          <p :class="page.subtitle">{{ handle }}</p>
        </div>
        <AvalonPresenceBadge :status="status" />
      </div>
      <p :class="styles.note">
        Presence only shows if this user has made it visible to you. More profile detail isn't
        shown to other users yet — see issue #403.
      </p>
    </AvalonCard>

    <p v-else :class="page.empty">That user couldn't be found.</p>
  </div>
</template>
