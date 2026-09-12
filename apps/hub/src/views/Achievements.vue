<script setup lang="ts">
// Achievements view (issue #35): every authentic claim the caller holds,
// across every issuer, shown with its own provenance and verification
// result — never ranked, scored, or collapsed when revoked (ADR #77,
// #81/#85). Reads only; issuing/revoking stays the issuer's own action via
// its own credentials, never something the Hub does on a player's behalf.
import { computed, onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { AvalonAchievementCard, AvalonCard, AvalonFilterBar } from '@avalon/ui'
import { formatActivityTimestamp } from '../api/activityFeed'
import {
  filterAchievementsByGame,
  listMyAchievements,
  sortAchievements,
} from '../api/achievements'
import type { Achievement, AchievementSort } from '../api/achievements'
import { useSessionStore } from '../stores/session'
import page from './page.module.scss'
import styles from './Achievements.module.scss'

const session = useSessionStore()
const router = useRouter()

const achievements = ref<Achievement[]>([])
const loading = ref(true)
const error = ref('')

const query = ref('')
const sort = ref<AchievementSort>('date')
const selectedGame = ref('')

onMounted(async () => {
  if (!session.token) return
  try {
    achievements.value = await listMyAchievements(session.token)
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    loading.value = false
  }
})

// One entry per distinct issuer present in the caller's own history — not
// a global game directory listing, so the filter never offers a game the
// player has no claims from.
const games = computed(() => {
  const bySlug = new Map<string, string>()
  for (const a of achievements.value) {
    if (!bySlug.has(a.issuerSlug)) bySlug.set(a.issuerSlug, a.issuerName ?? a.issuerSlug)
  }
  return [...bySlug.entries()].map(([slug, name]) => ({ slug, name }))
})

const visibleAchievements = computed(() => {
  const byName = achievements.value.filter((a) => {
    const haystack = `${a.achievementName ?? ''} ${a.issuerName ?? ''}`.toLowerCase()
    return haystack.includes(query.value.trim().toLowerCase())
  })
  const byGame = filterAchievementsByGame(byName, selectedGame.value || null)
  return sortAchievements(byGame, sort.value)
})

function onViewIssuer(slug: string) {
  router.push({ name: 'integration-profile', params: { slug } })
}
</script>

<template>
  <div v-if="!loading" :class="page.page">
    <header :class="page.pageHeader">
      <h1 :class="page.title">Achievements</h1>
      <p :class="page.subtitle">
        Every claim issued to you, across every game, app, and service — including ones that were
        later revoked.
      </p>
    </header>
    <p v-if="error" :class="page.error">{{ error }}</p>

    <AvalonCard :title="`Your achievements (${achievements.length})`">
      <template v-if="achievements.length > 0">
        <div :class="styles.filters">
          <AvalonFilterBar
            label="Search by achievement or game"
            placeholder="Dragon Slayer"
            :query="query"
            no-margin
            :sort-options="[
              { value: 'date', label: 'Date issued' },
              { value: 'name', label: 'Achievement name' },
              { value: 'game', label: 'Game' },
            ]"
            :sort-value="sort"
            @update:query="query = $event"
            @update:sort-value="sort = $event as AchievementSort"
          />
          <select v-model="selectedGame" :class="styles.gameSelect">
            <option value="">All games</option>
            <option v-for="game in games" :key="game.slug" :value="game.slug">{{ game.name }}</option>
          </select>
        </div>

        <p v-if="visibleAchievements.length === 0" :class="page.empty">
          No achievements match your filters.
        </p>
        <div v-else :class="styles.list">
          <AvalonAchievementCard
            v-for="achievement in visibleAchievements"
            :key="achievement.id"
            :achievement-name="achievement.achievementName ?? achievement.achievementRef"
            :icon="achievement.achievementIcon"
            :icon-url="achievement.achievementIconUrl"
            :issuer-name="achievement.issuerName ?? achievement.issuerSlug"
            :issuer-slug="achievement.issuerSlug"
            :issued-at="formatActivityTimestamp(achievement.issuedAt)"
            :status="achievement.status"
            :invalid-reason="achievement.invalidReason"
            :history="
              achievement.history.map((h) => ({ ...h, at: formatActivityTimestamp(h.at) }))
            "
            @view-issuer="onViewIssuer(achievement.issuerSlug)"
          />
        </div>
      </template>
      <p v-else :class="page.empty">
        No achievements yet — they'll show up here as soon as a connected game, app, or service
        issues you one.
      </p>
    </AvalonCard>
  </div>
</template>
