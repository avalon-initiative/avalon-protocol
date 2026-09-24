<script setup lang="ts">
// The persistent logged-in shell: a fixed sidebar on desktop,
// a bottom nav on mobile, a header with the caller's user chip, and a
// <RouterView /> for the page. Pages are nested child routes under this
// layout (see router/index.ts). Nav entries for features that don't exist
// yet (chat, discover) are rendered disabled with a "Soon" tag
// rather than hidden, so the layout reflects the roadmap honestly. Guilds
// and Integrators are no longer among them.
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { AvalonBottomNav, AvalonIcon, AvalonPresenceBadge, AvalonSidebarNav, AvalonUserChip } from '@avalon-initiative/common-ui'
import type { AvalonNavItem, PresenceStatus } from '@avalon-initiative/common-ui'
import {
  countUnread,
  isUnread,
  loadLastSeen,
  markChannelSeen,
  previewBody,
} from '../api/guildAnnouncements'
import type { GuildAnnouncementAlert } from '@avalon-initiative/protocol-sdk'
import NetworkStatus from '../components/NetworkStatus.vue'
import { useNotificationSummary } from '../composables/useNotificationSummary'
import { useSessionStore } from '../api/session'
import styles from '../styles/HubShell.module.scss'

// Re-publish well inside the server's 120s presence TTL so the user
// stays Online to their friends for as long as the Hub is open.
const PRESENCE_HEARTBEAT_MS = 60_000

// Issue #280: same polling cadence #389 already established for
// Achievements/Activity/Friends — no WebSocket needed for milestone 1.
const ANNOUNCEMENTS_POLL_INTERVAL_MS = 15_000

const route = useRoute()
const router = useRouter()
const session = useSessionStore()

const displayName = ref('')
const avatarUrl = ref<string | null>(null)
const myStatus = ref<PresenceStatus>('Offline')
const loading = ref(true)
const error = ref('')

let heartbeatHandle: ReturnType<typeof setInterval> | undefined
let announcementsPollHandle: ReturnType<typeof setInterval> | undefined

const announcementAlerts = ref<GuildAnnouncementAlert[]>([])
const lastSeenByChannel = ref(loadLastSeen())
const showAnnouncementsPanel = ref(false)
const unreadAnnouncementCount = computed(() =>
  countUnread(announcementAlerts.value, lastSeenByChannel.value),
)

async function refreshAnnouncements() {
  const s = session.session
  if (!s) return
  try {
    const result = await s.guildAnnouncements()
    announcementAlerts.value = Array.isArray(result) ? result : []
  } catch {
    // Best-effort, same posture as presence — the next poll retries.
  }
}

function toggleAnnouncementsPanel() {
  showAnnouncementsPanel.value = !showAnnouncementsPanel.value
}

// Issue #466 — one aggregate badge across every other pending-action
// source (friend requests, guild join requests/invites, device-grant
// approvals, guardian requests/designations, unread DMs). Deliberately a
// separate bell from the announcements one above: announcements are
// ambient chat activity, this is "something is waiting on you."
const {
  totalCount: pendingActionCount,
  incomingFriendRequestCount,
  guildJoinRequestCount,
  guildInviteCount,
  deviceGrantCount,
  guardianRequestCount,
  newGuardianOfCount,
  unreadDmCount,
} = useNotificationSummary()
const showPendingActionsPanel = ref(false)

function togglePendingActionsPanel() {
  showPendingActionsPanel.value = !showPendingActionsPanel.value
}

interface PendingActionRow {
  key: string
  label: string
  count: number
  to: string
}

const pendingActionRows = computed<PendingActionRow[]>(() =>
  [
    { key: 'friends', label: 'Friend requests', count: incomingFriendRequestCount.value, to: '/friends' },
    { key: 'join-requests', label: 'Guild join requests', count: guildJoinRequestCount.value, to: '/guilds' },
    { key: 'invites', label: 'Guild invites', count: guildInviteCount.value, to: '/guilds' },
    { key: 'device-grants', label: 'Device approval requests', count: deviceGrantCount.value, to: '/profile' },
    { key: 'guardian-requests', label: 'Recovery requests to approve', count: guardianRequestCount.value, to: '/profile' },
    { key: 'guardian-of', label: 'New guardian designations', count: newGuardianOfCount.value, to: '/profile' },
    { key: 'messages', label: 'Unread messages', count: unreadDmCount.value, to: '/messages' },
  ].filter((row) => row.count > 0),
)

function onSelectPendingAction(to: string) {
  showPendingActionsPanel.value = false
  router.push(to)
}

// Opening a specific alert is what "reads" it (see
// api/guildAnnouncements.ts's module doc comment) — the panel itself
// staying open doesn't mark anything seen, only actually following an
// alert to its channel does.
function onSelectAnnouncement(alert: GuildAnnouncementAlert) {
  markChannelSeen(alert.channelId, alert.sentAt)
  lastSeenByChannel.value = loadLastSeen()
  showAnnouncementsPanel.value = false
  router.push({ name: 'guild-channel', params: { id: alert.guildId, cid: alert.channelId } })
}

const GITHUB_PROFILE_URL = 'https://github.com/avalon-initiative'
// FIXME: still the personal-account repo — point this at
// github.com/avalon-initiative/avalon-protocol once the actual repo
// transfer happens, not before (issues/contributing/release links below
// all derive from this and would 404 against an org repo that doesn't
// exist yet).
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
  const s = session.session
  if (!s) return
  try {
    const presence = await s.updatePresence(status)
    myStatus.value = (presence?.status as PresenceStatus | undefined) ?? status
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
  const s = session.session
  if (!s) return
  try {
    await s.refreshProfile()
    const profile = s.profile()
    displayName.value = profile.displayName
    avatarUrl.value = profile.avatarUrl
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    loading.value = false
  }
  await publishPresence('Online')
  heartbeatHandle = setInterval(() => publishPresence(), PRESENCE_HEARTBEAT_MS)

  await refreshAnnouncements()
  announcementsPollHandle = setInterval(refreshAnnouncements, ANNOUNCEMENTS_POLL_INTERVAL_MS)
})

onUnmounted(() => {
  if (heartbeatHandle) clearInterval(heartbeatHandle)
  if (announcementsPollHandle) clearInterval(announcementsPollHandle)
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

// Closes a still-open panel when navigation happens some other way (a
// sidebar click, browser back/forward) rather than through
// onSelectAnnouncement itself.
watch(
  () => route.fullPath,
  () => {
    showAnnouncementsPanel.value = false
  },
)
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
        <div :class="styles.notifications">
          <div :class="styles.notificationGroup">
            <button
              :class="styles.iconButton"
              type="button"
              :aria-label="`Guild announcements${unreadAnnouncementCount > 0 ? ` (${unreadAnnouncementCount} unread)` : ''}`"
              @click="toggleAnnouncementsPanel"
            >
              <AvalonIcon name="bell" :size="18" />
              <span v-if="unreadAnnouncementCount > 0" :class="styles.unreadBadge">{{
                unreadAnnouncementCount
              }}</span>
            </button>
            <div v-if="showAnnouncementsPanel" :class="styles.announcementsPanel">
              <p v-if="announcementAlerts.length === 0" :class="styles.announcementsEmpty">
                No guild announcements yet.
              </p>
              <button
                v-for="alertItem in announcementAlerts"
                :key="alertItem.messageId"
                type="button"
                :class="[
                  styles.announcementItem,
                  { [styles.announcementUnread]: isUnread(alertItem, lastSeenByChannel) },
                ]"
                @click="onSelectAnnouncement(alertItem)"
              >
                <span :class="styles.announcementChannel">#{{ alertItem.channelName }}</span>
                <span :class="styles.announcementBody">{{ previewBody(alertItem.body) }}</span>
              </button>
            </div>
          </div>
          <div :class="styles.notificationGroup">
            <button
              :class="styles.iconButton"
              type="button"
              :aria-label="`Pending actions${pendingActionCount > 0 ? ` (${pendingActionCount} waiting)` : ''}`"
              @click="togglePendingActionsPanel"
            >
              <AvalonIcon name="activity" :size="18" />
              <span v-if="pendingActionCount > 0" :class="styles.unreadBadge">{{ pendingActionCount }}</span>
            </button>
            <div v-if="showPendingActionsPanel" :class="styles.announcementsPanel">
              <p v-if="pendingActionRows.length === 0" :class="styles.announcementsEmpty">
                Nothing waiting on you.
              </p>
              <button
                v-for="row in pendingActionRows"
                :key="row.key"
                type="button"
                :class="[styles.announcementItem, styles.announcementUnread]"
                @click="onSelectPendingAction(row.to)"
              >
                <span :class="styles.announcementChannel">{{ row.label }}</span>
                <span :class="styles.announcementBody">{{ row.count }}</span>
              </button>
            </div>
          </div>
        </div>
        <RouterLink v-if="!loading" to="/profile" :class="styles.userLink">
          <AvalonUserChip :name="displayName" :avatar-src="avatarUrl" />
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
            Avalon Initiative
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
