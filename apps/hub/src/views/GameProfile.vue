<script setup lang="ts">
// Per-game profile page (issue #270, first buildable slice of #90):
// GET /games/{slug}'s public fields plus GET /games/{slug}/registry's five
// metrics (#261), each rendered through AvalonMetricTile with its
// definition and class label — never a bare number. `status` renders
// through a visibly distinct badge whenever it isn't "active". No
// key-history UI here — that's #84/#48, blocked on the still-open #80.
// Public and unauthenticated: no session/token dependency in this view.
import { computed } from 'vue'
import { useRoute } from 'vue-router'
import { AvalonCard, AvalonMetricTile } from '@avalon/ui'
import { isActiveGameStatus } from '../api/games'
import { useGameProfile } from '../composables/useGameProfile'
import gameStyles from './GameProfile.module.scss'
import styles from './page.module.scss'

const route = useRoute()
const slug = computed(() => route.params.slug as string)
const { game, metrics, loading, error } = useGameProfile(slug)

function formatRegisteredAt(iso: string): string {
  return new Date(iso).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' })
}
</script>

<template>
  <div v-if="!loading && game" :class="styles.page">
    <header :class="styles.pageHeader">
      <div :class="gameStyles.heading">
        <h1 :class="styles.title">{{ game.name }}</h1>
        <span v-if="!isActiveGameStatus(game.status)" :class="gameStyles.statusBadge">{{ game.status }}</span>
      </div>
      <p :class="styles.subtitle">{{ game.developer }} · Registered {{ formatRegisteredAt(game.registered_at) }}</p>
    </header>

    <p v-if="error" :class="styles.error">{{ error }}</p>

    <AvalonCard title="Registry metrics" subtitle="Facts derived from protocol activity, shown exactly as the registry reports them.">
      <div :class="gameStyles.metricsGrid">
        <AvalonMetricTile
          v-for="metric in metrics"
          :key="metric.key"
          :label="metric.label"
          :value="metric.value"
          :definition="metric.definition"
          :metric-class="metric.class"
        />
      </div>
    </AvalonCard>
  </div>
</template>
