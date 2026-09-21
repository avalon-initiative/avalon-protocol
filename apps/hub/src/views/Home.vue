<script setup lang="ts">
// The landing page after login (issue #148/#312): welcome header, a
// featured/connected-integrators strip, quick actions, friends online, guilds,
// latest guild messages, and recent activity — every section backed by an
// API that already exists (integrators.ts/guilds.ts/guildChat.ts), no new
// endpoints added here.
import { computed, onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { AvalonAvatar, AvalonButton, AvalonCard, AvalonIcon, AvalonPresenceBadge } from '@avalon/ui'
import { formatActivityTimestamp, summarizeActivityEntry } from '../api/activityFeed'
import type { HistoryEntry } from '@avalon/sdk'
import { useFriendsPresence } from '../composables/useFriendsPresence'
import { useMyGuilds } from '../composables/useMyGuilds'
import { useMyConnections } from '../composables/useMyConnections'
import { useLatestGuildMessages } from '../composables/useLatestGuildMessages'
import { useSessionStore } from '../api/session'
import styles from '../styles/Home.module.scss'

const RECENT_ACTIVITY_LIMIT = 6
const HOME_GUILDS_LIMIT = 4
const HOME_MESSAGES_LIMIT = 5

const router = useRouter()
const session = useSessionStore()
const { onlineFriends, loading: friendsLoading } = useFriendsPresence()
const { guilds, loading: guildsLoading } = useMyGuilds()
const { bindings: connectedIntegrators, loading: integratorsLoading } = useMyConnections()
const { latestMessages, loading: messagesLoading } = useLatestGuildMessages(guilds)

const homeGuilds = computed(() => guilds.value.slice(0, HOME_GUILDS_LIMIT))
const homeMessages = computed(() => latestMessages.value.slice(0, HOME_MESSAGES_LIMIT))

// The most recently connected app/integrator, if any — the only honest ordering
// available (IntegratorBindingResponse has no last-played/session data yet), so
// "Recently Connected" means exactly that, not "most played" or a curated
// pick — renamed from "Featured Integrator" since it isn't curated and connected
// apps aren't only integrators.
const featuredIntegrator = computed(() =>
  [...connectedIntegrators.value].sort((a, b) => b.establishedAt.localeCompare(a.establishedAt))[0],
)

const displayName = ref('')
const recentActivity = ref<HistoryEntry[]>([])
const loading = ref(true)
const error = ref('')

onMounted(async () => {
  const s = session.session
  if (!s) return
  try {
    displayName.value = s.profile().displayName
    const history = await s.history()
    recentActivity.value = (Array.isArray(history) ? history : []).slice(0, RECENT_ACTIVITY_LIMIT)
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    loading.value = false
  }
})

const quickActions = [
  { label: 'Add a friend', description: 'By handle or identity id', icon: 'plus', to: '/friends' },
  { label: 'Set up this device', description: 'Signing key and recovery', icon: 'device', to: '/profile' },
  { label: 'Edit profile', description: 'Name and avatar', icon: 'profile', to: '/profile' },
] as const
</script>

<template>
  <div :class="styles.page">
    <header :class="styles.welcome">
      <h1 :class="styles.title">
        Welcome back<template v-if="displayName">, {{ displayName }}</template>
      </h1>
      <p :class="styles.tagline">Your integrators. Your community. Your identity. Across every world.</p>
    </header>
    <p v-if="error" :class="styles.error">{{ error }}</p>

    <AvalonCard v-if="!integratorsLoading" title="Recently Connected" :class="styles.featuredCard">
      <div v-if="featuredIntegrator" :class="styles.featured">
        <span :class="styles.featuredIcon"><AvalonIcon name="integrators" :size="28" /></span>
        <div :class="styles.featuredText">
          <p :class="styles.featuredLabel">{{ featuredIntegrator.name }}</p>
          <p :class="styles.featuredMeta">
            Connected {{ formatActivityTimestamp(featuredIntegrator.establishedAt) }}
          </p>
        </div>
        <AvalonButton
          label="View"
          variant="secondary"
          @click="router.push({ name: 'integration-profile', params: { slug: featuredIntegrator.slug } })"
        />
      </div>
      <div v-else :class="styles.featured">
        <span :class="styles.featuredIcon"><AvalonIcon name="discover" :size="28" /></span>
        <div :class="styles.featuredText">
          <p :class="styles.featuredLabel">Nothing connected yet</p>
          <p :class="styles.featuredMeta">Browse the directory to find your first app or integrator.</p>
        </div>
        <AvalonButton
          label="Browse Apps & Integrators"
          variant="secondary"
          @click="router.push({ name: 'integrations' })"
        />
      </div>
    </AvalonCard>

    <div :class="styles.grid">
      <div :class="styles.mainColumn">
        <AvalonCard title="Your Apps & Integrators">
          <template #action>
            <RouterLink to="/connections">View all</RouterLink>
          </template>
          <template v-if="!integratorsLoading && connectedIntegrators.length === 0">
            <p :class="styles.empty">You haven't connected anything yet.</p>
            <AvalonButton
              label="Browse Apps & Integrators"
              variant="secondary"
              @click="router.push({ name: 'integrations' })"
            />
          </template>
          <ul v-else :class="styles.integratorGrid">
            <li v-for="binding in connectedIntegrators" :key="binding.bindingId" :class="styles.integratorTile">
              <RouterLink
                :to="{ name: 'integration-profile', params: { slug: binding.slug } }"
                :class="styles.integratorTileLink"
              >
                <span :class="styles.integratorTileIcon"><AvalonIcon name="integrators" :size="20" /></span>
                <span :class="styles.integratorTileName">{{ binding.name }}</span>
              </RouterLink>
            </li>
          </ul>
        </AvalonCard>

        <AvalonCard title="Recent Activity">
          <template #action>
            <RouterLink to="/activity">View all</RouterLink>
          </template>
          <p v-if="!loading && recentActivity.length === 0" :class="styles.empty">
            No activity yet — your history starts as soon as the network processes your first event.
          </p>
          <ul v-else :class="styles.activityList">
            <li v-for="entry in recentActivity" :key="entry.eventId" :class="styles.activityEntry">
              <span :class="styles.activityIcon"><AvalonIcon name="activity" :size="16" /></span>
              <span :class="styles.activitySummary">{{ summarizeActivityEntry(entry) }}</span>
              <time :class="styles.activityTime" :datetime="entry.timestamp">
                {{ formatActivityTimestamp(entry.timestamp) }}
              </time>
            </li>
          </ul>
        </AvalonCard>
      </div>

      <div :class="styles.sideColumn">
        <AvalonCard title="Quick Actions">
          <ul :class="styles.actionList">
            <li v-for="action in quickActions" :key="action.label">
              <button :class="styles.action" type="button" @click="router.push(action.to)">
                <span :class="styles.actionIcon"><AvalonIcon :name="action.icon" :size="18" /></span>
                <span :class="styles.actionText">
                  <span :class="styles.actionLabel">{{ action.label }}</span>
                  <span :class="styles.actionDescription">{{ action.description }}</span>
                </span>
                <span :class="styles.actionChevron">›</span>
              </button>
            </li>
          </ul>
        </AvalonCard>

        <AvalonCard :title="`Friends Online (${onlineFriends.length})`">
          <template #action>
            <RouterLink to="/friends">View all</RouterLink>
          </template>
          <p v-if="!friendsLoading && onlineFriends.length === 0" :class="styles.empty">
            No friends online right now.
          </p>
          <ul v-else :class="styles.friendList">
            <li v-for="friend in onlineFriends" :key="friend.identityId" :class="styles.friend">
              <AvalonAvatar :name="friend.displayName ?? friend.identityId" size="md" />
              <span :class="styles.friendName">{{ friend.displayName ?? friend.identityId }}</span>
              <AvalonPresenceBadge :status="friend.status" />
            </li>
          </ul>
          <AvalonButton
            v-if="!friendsLoading && onlineFriends.length === 0"
            label="Find friends"
            variant="secondary"
            @click="router.push('/friends')"
          />
        </AvalonCard>

        <AvalonCard title="Guilds">
          <template #action>
            <RouterLink to="/guilds">View all</RouterLink>
          </template>
          <template v-if="!guildsLoading && guilds.length === 0">
            <p :class="styles.empty">You haven't joined a guild yet.</p>
            <AvalonButton
              label="Find a Guild"
              variant="secondary"
              @click="router.push({ name: 'guilds' })"
            />
          </template>
          <ul v-else :class="styles.guildList">
            <li v-for="guild in homeGuilds" :key="guild.id">
              <RouterLink
                :to="{ name: 'guild', params: { id: guild.id } }"
                :class="styles.guildRow"
              >
                <span :class="styles.guildIcon"><AvalonIcon name="guilds" :size="18" /></span>
                <span :class="styles.guildText">
                  <span :class="styles.guildName">{{ guild.name }}</span>
                  <span :class="styles.guildMeta"
                    >{{ guild.tag }} · {{ guild.memberCount }} members</span
                  >
                </span>
              </RouterLink>
            </li>
          </ul>
        </AvalonCard>

        <AvalonCard title="Latest Messages">
          <template #action>
            <RouterLink to="/guilds">View all</RouterLink>
          </template>
          <p v-if="!messagesLoading && homeMessages.length === 0" :class="styles.empty">
            No guild messages yet.
          </p>
          <ul v-else :class="styles.messageList">
            <li v-for="entry in homeMessages" :key="entry.message.id">
              <RouterLink
                :to="{ name: 'guild-channel', params: { id: entry.guildId, cid: entry.channelId } }"
                :class="styles.messageRow"
              >
                <span :class="styles.messageIcon"><AvalonIcon name="messages" :size="16" /></span>
                <span :class="styles.messageText">
                  <span :class="styles.messageGuild">{{ entry.guildName }}</span>
                  <span :class="styles.messageBody">{{ entry.message.body }}</span>
                </span>
                <time :class="styles.messageTime" :datetime="entry.message.sentAt">
                  {{ formatActivityTimestamp(entry.message.sentAt) }}
                </time>
              </RouterLink>
            </li>
          </ul>
        </AvalonCard>
      </div>
    </div>
  </div>
</template>
