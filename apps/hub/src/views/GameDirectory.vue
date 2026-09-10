<script setup lang="ts">
// Game directory (issue #270, first buildable slice of #90): every
// registered game, from GET /games, with a search box and an explicit,
// neutral sort (name / newest) — no ranking, no score, no "recommended"
// ordering, per #89's own invariant. Public and unauthenticated: no
// session/token dependency anywhere in this view.
import { useRouter } from 'vue-router'
import { AvalonButton, AvalonCard, AvalonFilterBar, AvalonGameCard } from '@avalon/ui'
import type { AvalonFilterBarSortOption } from '@avalon/ui'
import { useDiscoverGames } from '../composables/useDiscoverGames'
import styles from './page.module.scss'

const router = useRouter()
const discover = useDiscoverGames()
discover.refresh()

const sortOptions: AvalonFilterBarSortOption[] = [
  { value: 'newest', label: 'Newest' },
  { value: 'name', label: 'Name' },
]

function formatRegisteredAt(iso: string): string {
  return new Date(iso).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' })
}

function openGame(slug: string) {
  router.push({ name: 'game-profile', params: { slug } })
}
</script>

<template>
  <div :class="styles.page">
    <header :class="styles.pageHeader">
      <h1 :class="styles.title">Games</h1>
      <p :class="styles.subtitle">Games connected to Avalon — browse by name or see the newest arrivals.</p>
    </header>

    <AvalonCard title="Game directory">
      <AvalonFilterBar
        label="Search by name, slug, or developer"
        placeholder="Ashen Realms"
        :query="discover.query.value"
        :sort-options="sortOptions"
        :sort-value="discover.sort.value"
        @update:query="discover.query.value = $event"
        @update:sort-value="discover.sort.value = $event as 'newest' | 'name'"
      />
      <p v-if="discover.error.value" :class="styles.error">{{ discover.error.value }}</p>
      <p v-else-if="!discover.loading.value && discover.games.value.length === 0" :class="styles.empty">
        No games match your search.
      </p>
      <AvalonGameCard
        v-for="game in discover.games.value"
        :key="game.id"
        :name="game.name"
        :slug="game.slug"
        :developer="game.developer"
        :status="game.status"
        :registered-at="formatRegisteredAt(game.registered_at)"
        @select="openGame(game.slug)"
      />
      <AvalonButton
        v-if="discover.nextCursor.value"
        label="Load more"
        variant="secondary"
        @click="discover.loadMore()"
      />
    </AvalonCard>
  </div>
</template>
