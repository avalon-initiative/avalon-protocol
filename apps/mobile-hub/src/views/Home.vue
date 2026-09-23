<script setup lang="ts">
// The signed-in placeholder screen: proves the shared session/API wiring
// works end to end — guild chat/friends/presence views are separate,
// later work, not part of wiring the API itself.
import { onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { AvalonAvatar, AvalonButton, AvalonCard } from '@avalon/ui'
import { getMe, useSessionStore } from '@avalon/api-client'
import type { ProfileResponse } from '@avalon/api-client'
import styles from '../styles/Home.module.scss'

const router = useRouter()
const session = useSessionStore()

const profile = ref<ProfileResponse | null>(null)
const error = ref('')

onMounted(async () => {
  if (!session.token) return
  try {
    // Profile data is always re-read from GET /me, never cached locally
    // (#55's invariant) — the only thing this device persists is the
    // session token itself.
    profile.value = await getMe(session.token)
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Could not load your profile.'
  }
})

async function onLogout() {
  await session.logout()
  await router.push({ name: 'login' })
}
</script>

<template>
  <main :class="styles.main">
    <AvalonCard title="Avalon">
      <div v-if="profile" :class="styles.profile">
        <AvalonAvatar :name="profile.display_name" :src="profile.avatar_url" size="lg" />
        <p :class="styles.name">{{ profile.display_name }}</p>
        <p :class="styles.id">{{ profile.identity_id }}</p>
      </div>
      <p v-else-if="error" :class="styles.error">{{ error }}</p>
      <p v-else :class="styles.loading">Loading…</p>
      <p :class="styles.hint">Your guild, wherever you are.</p>
      <div :class="styles.actions">
        <AvalonButton label="Settings" variant="secondary" @click="router.push({ name: 'settings' })" />
        <AvalonButton label="Log out" variant="secondary" @click="onLogout" />
      </div>
    </AvalonCard>
  </main>
</template>
