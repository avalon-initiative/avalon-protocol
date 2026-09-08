<script setup lang="ts">
// The persistent logged-in shell (issue #130): identity header + tab nav,
// always visible regardless of which tab is active. Profile/Friends (and
// whatever future tabs land — Guild, Chat, Achievements) render inside
// <RouterView /> as nested routes under this layout rather than as their
// own standalone pages. See docs/architecture/hub.md for the pattern
// future Hub UI tickets should build against.
import { computed, onMounted, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { AvalonButton, AvalonTabs } from '@avalon/ui'
import type { AvalonTabItem } from '@avalon/ui'
import { getMe } from '../api/client'
import { useSessionStore } from '../stores/session'
import styles from './HubShell.module.scss'

const route = useRoute()
const router = useRouter()
const session = useSessionStore()

const displayName = ref('')
const identityId = ref('')
const loading = ref(true)
const error = ref('')

onMounted(async () => {
  if (!session.token) return
  try {
    const profile = await getMe(session.token)
    displayName.value = profile.display_name
    identityId.value = profile.identity_id
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    loading.value = false
  }
})

// Real tabs today are exactly Profile and Friends — add a new entry here
// (plus a matching child route) when a future ticket (#24 guild, #35
// achievements, #105 DMs) adds real tab content. Don't add a tab for
// something that has no route/content yet.
const tabs = computed<AvalonTabItem[]>(() => [
  { label: 'Profile', to: '/profile', active: route.path === '/profile' },
  { label: 'Friends', to: '/friends', active: route.path === '/friends' },
])

function onSelectTab(to: string) {
  router.push(to)
}

async function onLogout() {
  session.logout()
  await router.push({ name: 'login' })
}
</script>

<template>
  <div :class="styles.shell">
    <header v-if="!loading" :class="styles.header">
      <div :class="styles.identity">
        <span :class="styles.displayName">{{ displayName }}</span>
        <span :class="styles.identityId">{{ identityId }}</span>
      </div>
      <AvalonButton label="Log out" variant="secondary" @click="onLogout" />
    </header>
    <p v-if="error">{{ error }}</p>
    <AvalonTabs :tabs="tabs" @select="onSelectTab" />
    <main :class="styles.content">
      <RouterView />
    </main>
  </div>
</template>
