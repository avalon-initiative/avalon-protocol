<script setup lang="ts">
// The landing page after login (issue #148/#312): welcome header, a
// featured/connected-games strip, quick actions, friends online, guilds,
// latest guild messages, and recent activity — every section backed by an
// API that already exists (games.ts/guilds.ts/guildChat.ts), no new
// endpoints added here.
import { computed, onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { AvalonAvatar, AvalonButton, AvalonCard, AvalonIcon, AvalonPresenceBadge } from '@avalon/ui'
import { getMe, getMyHistory } from '../api/client'
import { formatActivityTimestamp, summarizeActivityEntry } from '../api/activityFeed'
import type { HistoryEntryResponse } from '../api/types'
import { useFriendsPresence } from '../composables/useFriendsPresence'
import { useMyGuilds } from '../composables/useMyGuilds'
import { useMyConnections } from '../composables/useMyConnections'
import { useLatestGuildMessages } from '../composables/useLatestGuildMessages'
import { useSessionStore } from '../stores/session'
import styles from './Home.module.scss'

const RECENT_ACTIVITY_LIMIT = 6
const HOME_GUILDS_LIMIT = 4
const HOME_MESSAGES_LIMIT = 5

const router = useRouter()
const session = useSessionStore()
const { onlineFriends, loading: friendsLoading } = useFriendsPresence()
const { guilds, loading: guildsLoading } = useMyGuilds()
const { bindings: connectedGames, loading: gamesLoading } = useMyConnections()
const { latestMessages, loading: messagesLoading } = useLatestGuildMessages(guilds)

const homeGuilds = computed(() => guilds.value.slice(0, HOME_GUILDS_LIMIT))
const homeMessages = computed(() => latestMessages.value.slice(0, HOME_MESSAGES_LIMIT))

// The most recently connected game, if any — the only honest ordering
// available (GameBindingResponse has no last-played/session data yet), so
// "Featured" means "newest connection", not "most played".
const featuredGame = computed(() =>
  [...connectedGames.value].sort((a, b) => b.established_at.localeCompare(a.established_at))[0],
)

const displayName = ref('')
const recentActivity = ref<HistoryEntryResponse[]>([])
const loading = ref(true)
const error = ref('')

onMounted(async () => {
  if (!session.token) return
  try {
    const [profile, history] = await Promise.all([
      getMe(session.token),
      getMyHistory(session.token),
    ])
    displayName.value = profile.display_name
    recentActivity.value = history.slice(0, RECENT_ACTIVITY_LIMIT)
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
      <p :class="styles.tagline">Your games. Your community. Your identity. Across every world.</p>
    </header>
    <p v-if="error" :class="styles.error">{{ error }}</p>

    <AvalonCard v-if="!gamesLoading" title="Featured Game" :class="styles.featuredCard">
      <div v-if="featuredGame" :class="styles.featured">
        <span :class="styles.featuredIcon"><AvalonIcon name="games" :size="28" /></span>
        <div :class="styles.featuredText">
          <p :class="styles.featuredLabel">{{ featuredGame.name }}</p>
          <p :class="styles.featuredMeta">
            Connected {{ formatActivityTimestamp(featuredGame.established_at) }}
          </p>
        </div>
        <AvalonButton
          label="View Game"
          variant="secondary"
          @click="router.push({ name: 'integration-profile', params: { slug: featuredGame.slug } })"
        />
      </div>
      <div v-else :class="styles.featured">
        <span :class="styles.featuredIcon"><AvalonIcon name="discover" :size="28" /></span>
        <div :class="styles.featuredText">
          <p :class="styles.featuredLabel">No games connected yet</p>
          <p :class="styles.featuredMeta">Browse the directory to find your first game.</p>
        </div>
        <AvalonButton
          label="Explore Games"
          variant="secondary"
          @click="router.push({ name: 'integrations' })"
        />
      </div>
    </AvalonCard>

    <div :class="styles.grid">
      <div :class="styles.mainColumn">
        <AvalonCard title="Your Games">
          <template #action>
            <RouterLink to="/connections">View all</RouterLink>
          </template>
          <template v-if="!gamesLoading && connectedGames.length === 0">
            <p :class="styles.empty">You haven't connected to any games yet.</p>
            <AvalonButton
              label="Explore Games"
              variant="secondary"
              @click="router.push({ name: 'integrations' })"
            />
          </template>
          <ul v-else :class="styles.gameGrid">
            <li v-for="binding in connectedGames" :key="binding.binding_id" :class="styles.gameTile">
              <RouterLink
                :to="{ name: 'integration-profile', params: { slug: binding.slug } }"
                :class="styles.gameTileLink"
              >
                <span :class="styles.gameTileIcon"><AvalonIcon name="games" :size="20" /></span>
                <span :class="styles.gameTileName">{{ binding.name }}</span>
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
            <li v-for="entry in recentActivity" :key="entry.event_id" :class="styles.activityEntry">
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
                    >{{ guild.tag }} · {{ guild.member_count }} members</span
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
                <time :class="styles.messageTime" :datetime="entry.message.sent_at">
                  {{ formatActivityTimestamp(entry.message.sent_at) }}
                </time>
              </RouterLink>
            </li>
          </ul>
        </AvalonCard>
      </div>
    </div>
  </div>
</template>
