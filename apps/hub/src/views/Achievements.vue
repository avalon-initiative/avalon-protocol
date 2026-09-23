<script setup lang="ts">
// Achievements view: every authentic claim the caller holds,
// across every issuer, shown with its own provenance and verification
// result — never ranked, scored, or collapsed when revoked. Reads only;
// issuing/revoking stays the issuer's own action via
// its own credentials, never something the Hub does on a user's behalf.
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { AvalonAchievementCard, AvalonCard, AvalonFilterBar } from '@avalon/ui'
import { formatActivityTimestamp } from '../api/activityFeed'
import {
  filterAchievementsByIntegrator,
  listMyAchievements,
  sortAchievements,
} from '../api/achievements'
import type { Achievement, AchievementSort } from '../api/achievements'
import { useSessionStore } from '../api/session'
import page from '../styles/page.module.scss'
import styles from '../styles/Achievements.module.scss'

// Issue #389: this view used to load once on mount and never refresh —
// polling on the same interval useConversations/useGuildChat already
// established for milestone 1 (no WebSocket needed) rather than adding a
// third, different cadence.
const POLL_INTERVAL_MS = 15_000

const session = useSessionStore()
const router = useRouter()

const achievements = ref<Achievement[]>([])
const loading = ref(true)
const error = ref('')

const query = ref('')
const sort = ref<AchievementSort>('date')
const selectedIntegrator = ref('')

let pollHandle: ReturnType<typeof setInterval> | undefined

async function refresh() {
  const s = session.session
  if (!s) return
  try {
    achievements.value = await listMyAchievements(s)
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}

onMounted(async () => {
  await refresh()
  loading.value = false
  pollHandle = setInterval(refresh, POLL_INTERVAL_MS)
})

onUnmounted(() => {
  if (pollHandle) clearInterval(pollHandle)
})

// One entry per distinct issuer present in the caller's own history — not
// a global integrator directory listing, so the filter never offers an integrator the
// user has no claims from.
const integrators = computed(() => {
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
  const byIntegrator = filterAchievementsByIntegrator(byName, selectedIntegrator.value || null)
  return sortAchievements(byIntegrator, sort.value)
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
        Every claim issued to you, across every integrator, app, and service — including ones that were
        later revoked.
      </p>
    </header>
    <p v-if="error" :class="page.error">{{ error }}</p>

    <AvalonCard :title="`Your achievements (${achievements.length})`">
      <template v-if="achievements.length > 0">
        <div :class="styles.filters">
          <AvalonFilterBar
            label="Search by achievement or integrator"
            placeholder="Dragon Slayer"
            :query="query"
            no-margin
            :sort-options="[
              { value: 'date', label: 'Date issued' },
              { value: 'name', label: 'Achievement name' },
              { value: 'game', label: 'Integrator' },
            ]"
            :sort-value="sort"
            @update:query="query = $event"
            @update:sort-value="sort = $event as AchievementSort"
          />
          <select v-model="selectedIntegrator" :class="styles.integratorSelect">
            <option value="">All integrators</option>
            <option v-for="integrator in integrators" :key="integrator.slug" :value="integrator.slug">{{ integrator.name }}</option>
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
        No achievements yet — they'll show up here as soon as a connected integrator, app, or service
        issues you one.
      </p>
    </AvalonCard>
  </div>
</template>
