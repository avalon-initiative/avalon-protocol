<script setup lang="ts">
// The landing page after login (issue #148), built only from what exists:
// a welcome header, quick actions, friends online, and recent activity.
// Game/guild/message cards arrive with those features, not here.
import { onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { AvalonAvatar, AvalonButton, AvalonCard, AvalonIcon, AvalonPresenceBadge } from '@avalon/ui'
import { getMe, getMyHistory } from '../api/client'
import { formatActivityTimestamp, summarizeActivityEntry } from '../api/activityFeed'
import type { HistoryEntryResponse } from '../api/types'
import { useFriendsPresence } from '../composables/useFriendsPresence'
import { useSessionStore } from '../stores/session'
import styles from './Home.module.scss'

const RECENT_ACTIVITY_LIMIT = 6

const router = useRouter()
const session = useSessionStore()
const { onlineFriends, loading: friendsLoading } = useFriendsPresence()

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

    <div :class="styles.grid">
      <div :class="styles.mainColumn">
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
      </div>
    </div>
  </div>
</template>
