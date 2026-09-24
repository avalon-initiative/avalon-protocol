<script setup lang="ts">
// The signed-in placeholder screen: proves the shared session/API wiring
// works end to end — guild chat/friends/presence views are separate,
// later work, not part of wiring the API itself.
import { onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { AvalonAvatar, AvalonButton, AvalonCard } from '@avalon-initiative/common-ui'
import type { Profile } from '@avalon-initiative/protocol-sdk'
import { useSessionStore } from '../api/session'
import styles from '../styles/Home.module.scss'

const router = useRouter()
const session = useSessionStore()

const profile = ref<Profile | null>(null)
const error = ref('')

onMounted(async () => {
  const active = session.session
  if (!active) return
  try {
    // Profile data is always re-read from GET /me, never cached locally; the
    // only things this device persists are the session credentials.
    await active.refreshProfile()
    profile.value = active.profile()
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
        <AvalonAvatar :name="profile.displayName" :src="profile.avatarUrl" size="lg" />
        <p :class="styles.name">{{ profile.displayName }}</p>
        <p :class="styles.id">{{ profile.identityId }}</p>
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
