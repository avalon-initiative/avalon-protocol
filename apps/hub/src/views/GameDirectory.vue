<script setup lang="ts">
// Integrator directory (#270, generalized in #282). Apps/Services tabs
// render empty rather than hidden — no real registrants yet.
import { computed, ref } from 'vue'
import { useRouter } from 'vue-router'
import { AvalonButton, AvalonCard, AvalonFilterBar, AvalonGameCard } from '@avalon/ui'
import type { AvalonFilterBarSortOption } from '@avalon/ui'
import { useDiscoverGames } from '../composables/useDiscoverGames'
import type { IntegratorCategory } from '../api/types'
import gameDirectoryStyles from './GameDirectory.module.scss'
import styles from './page.module.scss'

const router = useRouter()
const discover = useDiscoverGames()
discover.refresh()

const sortOptions: AvalonFilterBarSortOption[] = [
  { value: 'newest', label: 'Newest' },
  { value: 'name', label: 'Name' },
]

const categoryTabs: { value: IntegratorCategory; label: string }[] = [
  { value: 'game', label: 'Games' },
  { value: 'app', label: 'Apps' },
  { value: 'service', label: 'Services' },
]
const activeCategory = ref<IntegratorCategory>('game')

const visibleGames = computed(() =>
  discover.games.value.filter((game) => game.category === activeCategory.value),
)

function formatRegisteredAt(iso: string): string {
  return new Date(iso).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' })
}

function openGame(slug: string) {
  router.push({ name: 'integration-profile', params: { slug } })
}
</script>

<template>
  <div :class="styles.page">
    <header :class="styles.pageHeader">
      <h1 :class="styles.title">Connected Apps</h1>
      <p :class="styles.subtitle">Games, apps, and services connected to Avalon — browse by name or see the newest arrivals.</p>
    </header>

    <AvalonCard title="Directory">
      <div :class="gameDirectoryStyles.tabs" role="tablist">
        <button
          v-for="tab in categoryTabs"
          :key="tab.value"
          type="button"
          role="tab"
          :aria-selected="activeCategory === tab.value"
          :class="[gameDirectoryStyles.tab, { [gameDirectoryStyles.tabActive]: activeCategory === tab.value }]"
          @click="activeCategory = tab.value"
        >
          {{ tab.label }}
        </button>
      </div>

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
      <p v-else-if="!discover.loading.value && visibleGames.length === 0" :class="styles.empty">
        No {{ categoryTabs.find((t) => t.value === activeCategory)?.label.toLowerCase() }} match your search.
      </p>
      <AvalonGameCard
        v-for="game in visibleGames"
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
