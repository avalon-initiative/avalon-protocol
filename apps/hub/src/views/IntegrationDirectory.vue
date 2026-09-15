<script setup lang="ts">
// Integrator directory (#270, generalized in #282). Apps/Services tabs
// render empty rather than hidden — no real registrants yet.
import { computed, ref } from 'vue'
import { useRouter } from 'vue-router'
import { AvalonButton, AvalonCard, AvalonFilterBar, AvalonIntegratorCard } from '@avalon/ui'
import type { AvalonFilterBarSortOption } from '@avalon/ui'
import { useDiscoverIntegrations } from '../composables/useDiscoverIntegrations'
import { useMyConnections } from '../composables/useMyConnections'
import { useSessionStore } from '../stores/session'
import type { IntegratorCategory } from '../api/types'
import integratorDirectoryStyles from '../styles/IntegrationDirectory.module.scss'
import styles from '../styles/page.module.scss'

const router = useRouter()
const session = useSessionStore()
const discover = useDiscoverIntegrations()
discover.refresh()
// Issue #467: which cards get a "Connect" action — only when there's a
// session to bind with, and only for integrators not already connected.
const myConnections = useMyConnections()
function isConnected(slug: string): boolean {
  return myConnections.bindings.value.some((b) => b.slug === slug)
}
function onConnect(slug: string) {
  router.push({ name: 'connect-integrator', params: { slug } })
}

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

const visibleIntegrators = computed(() =>
  discover.integrators.value.filter((integrator) => integrator.category === activeCategory.value),
)

function formatRegisteredAt(iso: string): string {
  return new Date(iso).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' })
}

function openIntegrator(slug: string) {
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
      <div :class="integratorDirectoryStyles.tabs" role="tablist">
        <button
          v-for="tab in categoryTabs"
          :key="tab.value"
          type="button"
          role="tab"
          :aria-selected="activeCategory === tab.value"
          :class="[integratorDirectoryStyles.tab, { [integratorDirectoryStyles.tabActive]: activeCategory === tab.value }]"
          @click="activeCategory = tab.value"
        >
          {{ tab.label }}
        </button>
      </div>

      <AvalonFilterBar
        label="Search by name, slug, or owner"
        placeholder="Ashen Realms"
        :query="discover.query.value"
        :sort-options="sortOptions"
        :sort-value="discover.sort.value"
        @update:query="discover.query.value = $event"
        @update:sort-value="discover.sort.value = $event as 'newest' | 'name'"
      />
      <p v-if="discover.error.value" :class="styles.error">{{ discover.error.value }}</p>
      <p v-else-if="!discover.loading.value && visibleIntegrators.length === 0" :class="styles.empty">
        No {{ categoryTabs.find((t) => t.value === activeCategory)?.label.toLowerCase() }} match your search.
      </p>
      <div
        v-for="integrator in visibleIntegrators"
        :key="integrator.id"
        :class="integratorDirectoryStyles.cardRow"
      >
        <AvalonIntegratorCard
          :name="integrator.name"
          :slug="integrator.slug"
          :owner-name="integrator.owner_name"
          :status="integrator.status"
          :registered-at="formatRegisteredAt(integrator.registered_at)"
          @select="openIntegrator(integrator.slug)"
        />
        <AvalonButton
          v-if="session.isAuthenticated() && !isConnected(integrator.slug)"
          label="Connect"
          variant="secondary"
          @click="onConnect(integrator.slug)"
        />
      </div>
      <AvalonButton
        v-if="discover.nextCursor.value"
        label="Load more"
        variant="secondary"
        @click="discover.loadMore()"
      />
    </AvalonCard>
  </div>
</template>
