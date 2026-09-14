<script setup lang="ts">
// The persistent logged-in shell (issue #148): a fixed sidebar on desktop,
// a bottom nav on mobile, a header with the caller's user chip, and a
// <RouterView /> for the page. Pages are nested child routes under this
// layout (see router/index.ts). Nav entries for features that don't exist
// yet (chat, discover) are rendered disabled with a "Soon" tag
// rather than hidden, so the layout reflects the roadmap honestly. Guilds
// (issue #24) and Integrators (issue #270) are no longer among them.
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { AvalonBottomNav, AvalonIcon, AvalonPresenceBadge, AvalonSidebarNav, AvalonUserChip } from '@avalon/ui'
import type { AvalonNavItem, PresenceStatus } from '@avalon/ui'
import { getMe, updateMyPresence } from '../api/client'
import NetworkStatus from '../components/NetworkStatus.vue'
import { useSessionStore } from '../stores/session'
import styles from './HubShell.module.scss'

// Re-publish well inside the server's 120s presence TTL so the user
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

const GITHUB_PROFILE_URL = 'https://github.com/LunarVagabond'
const GITHUB_REPO_URL = 'https://github.com/LunarVagabond/avalon-protocol'
const GITHUB_ISSUES_URL = `${GITHUB_REPO_URL}/issues/new?template=bug_report.yml`
const CONTRIBUTING_URL = `${GITHUB_REPO_URL}/blob/main/.github/CONTRIBUTING.md`

// `__BUILD_REVISION__`/`__BUILD_IS_RELEASE__` come from vite.config.ts's
// `define` — the exact tag when built from a release, a short commit hash otherwise.
const buildRevision = __BUILD_REVISION__
const revisionUrl =
  buildRevision === 'unknown'
    ? null
    : __BUILD_IS_RELEASE__
      ? `${GITHUB_REPO_URL}/releases/tag/${buildRevision}`
      : `${GITHUB_REPO_URL}/commit/${buildRevision}`

// Issue #390: `PUT /me/presence` treats Away/DoNotDisturb/Offline as sticky
// manual overrides that persist until the caller explicitly sets Online
// again — so the heartbeat re-publishes whatever status the user last
// chose (`myStatus`), not a hardcoded 'Online', or it would silently
// overwrite a manual Away/DND/Offline choice on the next tick.
async function publishPresence(status: PresenceStatus = myStatus.value) {
  if (!session.token) return
  try {
    const presence = await updateMyPresence(session.token, { status })
    myStatus.value = presence?.status ?? status
  } catch {
    // Presence is best-effort; the next heartbeat retries.
  }
}

const STATUS_OPTIONS: PresenceStatus[] = ['Online', 'Away', 'DoNotDisturb', 'Offline']

async function onSelectStatus(event: Event) {
  const status = (event.target as HTMLSelectElement).value as PresenceStatus
  await publishPresence(status)
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
  await publishPresence('Online')
  heartbeatHandle = setInterval(() => publishPresence(), PRESENCE_HEARTBEAT_MS)
})

onUnmounted(() => {
  if (heartbeatHandle) clearInterval(heartbeatHandle)
})

const navItems = computed<AvalonNavItem[]>(() => [
  { label: 'Home', to: '/home', icon: 'home', active: route.path === '/home' },
  {
    label: 'Connected Apps',
    to: '/integrations',
    icon: 'integrators',
    active: route.path.startsWith('/integrations') || route.path.startsWith('/integrations'),
  },
  { label: 'Guilds', to: '/guilds', icon: 'guilds', active: route.path.startsWith('/guilds') },
  { label: 'Friends', to: '/friends', icon: 'friends', active: route.path === '/friends' },
  {
    label: 'Achievements',
    to: '/achievements',
    icon: 'achievements',
    active: route.path === '/achievements',
  },
  { label: 'Messages', to: '/messages', icon: 'chat', active: route.path.startsWith('/messages') },
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
        <select
          :class="styles.statusSelect"
          aria-label="Set your status"
          :value="myStatus"
          @change="onSelectStatus"
        >
          <option v-for="status in STATUS_OPTIONS" :key="status" :value="status">
            {{ status === 'DoNotDisturb' ? 'Do Not Disturb' : status }}
          </option>
        </select>
        <NetworkStatus />
      </div>
    </aside>

    <div :class="styles.body">
      <header :class="styles.header">
        <div :class="styles.search">
          <AvalonIcon name="search" :size="16" />
          <input
            :class="styles.searchInput"
            type="search"
            placeholder="Search integrators, users, guilds… (coming soon)"
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
        <div :class="styles.contentInner">
          <RouterView />
        </div>
      </main>
      <footer :class="styles.footer">
        <div :class="styles.footerLeft">
          <a :class="styles.footerLink" :href="GITHUB_PROFILE_URL" target="_blank" rel="noopener noreferrer">
            @LunarVagabond
          </a>
        </div>

        <div :class="styles.footerCenter">
          <a
            v-if="revisionUrl"
            :class="styles.footerRevision"
            :href="revisionUrl"
            target="_blank"
            rel="noopener noreferrer"
          >
            AvalonHUB · {{ buildRevision }}
          </a>
          <span v-else :class="styles.footerRevision">AvalonHUB · {{ buildRevision }}</span>
        </div>

        <div :class="styles.footerRight">
          <a :class="styles.footerLink" :href="GITHUB_ISSUES_URL" target="_blank" rel="noopener noreferrer">
            Found a bug? Report it!
          </a>
          <span :class="styles.footerSep">|</span>
          <a :class="styles.footerLink" :href="CONTRIBUTING_URL" target="_blank" rel="noopener noreferrer">
            Contribute
          </a>
        </div>
      </footer>
    </div>

    <div :class="styles.bottomNav">
      <AvalonBottomNav :items="bottomNavItems" @select="onSelectNav" />
    </div>
  </div>
</template>
