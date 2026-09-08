<script setup lang="ts">
// The persistent logged-in shell (issue #148): a fixed sidebar on desktop,
// a bottom nav on mobile, a header with the caller's user chip, and a
// <RouterView /> for the page. Pages are nested child routes under this
// layout (see router/index.ts). Nav entries for features that don't exist
// yet (games, chat, discover) are rendered disabled with a "Soon" tag
// rather than hidden, so the layout reflects the roadmap honestly. Guilds
// (issue #24) is no longer one of them.
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { AvalonBottomNav, AvalonIcon, AvalonPresenceBadge, AvalonSidebarNav, AvalonUserChip } from '@avalon/ui'
import type { AvalonNavItem, PresenceStatus } from '@avalon/ui'
import { getMe, updateMyPresence } from '../api/client'
import { useSessionStore } from '../stores/session'
import styles from './HubShell.module.scss'

// Re-publish well inside the server's 120s presence TTL so the player
// stays Online to their friends for as long as the Hub is open.
const PRESENCE_HEARTBEAT_MS = 60_000

const route = useRoute()
const router = useRouter()
const session = useSessionStore()

const displayName = ref('')
const handle = ref('')
const avatarUrl = ref<string | null>(null)
const myStatus = ref<PresenceStatus>('Offline')
const loading = ref(true)
const error = ref('')

let heartbeatHandle: ReturnType<typeof setInterval> | undefined

async function publishPresence() {
  if (!session.token) return
  try {
    const presence = await updateMyPresence(session.token, { status: 'Online' })
    myStatus.value = presence?.status ?? 'Online'
  } catch {
    // Presence is best-effort; the next heartbeat retries.
  }
}

onMounted(async () => {
  if (!session.token) return
  try {
    const profile = await getMe(session.token)
    displayName.value = profile.display_name
    handle.value = profile.handle
    avatarUrl.value = profile.avatar_url
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    loading.value = false
  }
  await publishPresence()
  heartbeatHandle = setInterval(publishPresence, PRESENCE_HEARTBEAT_MS)
})

onUnmounted(() => {
  if (heartbeatHandle) clearInterval(heartbeatHandle)
})

const navItems = computed<AvalonNavItem[]>(() => [
  { label: 'Home', to: '/home', icon: 'home', active: route.path === '/home' },
  { label: 'Games', to: '/games', icon: 'games', active: false, disabled: true },
  { label: 'Guilds', to: '/guilds', icon: 'guilds', active: route.path.startsWith('/guilds') },
  { label: 'Friends', to: '/friends', icon: 'friends', active: route.path === '/friends' },
  { label: 'Chat', to: '/chat', icon: 'chat', active: false, disabled: true },
  { label: 'Discover', to: '/discover', icon: 'discover', active: false, disabled: true },
  { label: 'Profile', to: '/profile', icon: 'profile', active: route.path === '/profile' },
])

// Five slots on mobile, mirroring the mock's bottom bar.
const bottomNavItems = computed<AvalonNavItem[]>(() =>
  navItems.value.filter((item) => ['/home', '/friends', '/guilds', '/chat', '/profile'].includes(item.to)),
)

function onSelectNav(to: string) {
  router.push(to)
}
</script>

<template>
  <div :class="styles.shell">
    <aside :class="styles.sidebar">
      <div :class="styles.brand">
        <span :class="styles.brandMark"><AvalonIcon name="logo" :size="22" /></span>
        <span :class="styles.brandName">AVALON</span>
      </div>
      <AvalonSidebarNav :items="navItems" @select="onSelectNav" />
      <div :class="styles.sidebarFooter">
        <AvalonPresenceBadge :status="myStatus" />
        <span :class="styles.connected">Connected to Avalon</span>
      </div>
    </aside>

    <div :class="styles.body">
      <header :class="styles.header">
        <div :class="styles.search">
          <AvalonIcon name="search" :size="16" />
          <input
            :class="styles.searchInput"
            type="search"
            placeholder="Search games, players, guilds… (coming soon)"
            disabled
          />
        </div>
        <button :class="styles.iconButton" type="button" aria-label="Notifications (coming soon)" disabled>
          <AvalonIcon name="bell" :size="18" />
        </button>
        <RouterLink v-if="!loading" to="/profile" :class="styles.userLink">
          <AvalonUserChip :name="displayName" :detail="handle" :avatar-src="avatarUrl" />
        </RouterLink>
      </header>
      <p v-if="error" :class="styles.error">{{ error }}</p>
      <main :class="styles.content">
        <RouterView />
      </main>
    </div>

    <div :class="styles.bottomNav">
      <AvalonBottomNav :items="bottomNavItems" @select="onSelectNav" />
    </div>
  </div>
</template>
