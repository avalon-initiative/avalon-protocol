<script setup lang="ts">
// Per-game profile page (issue #270, first buildable slice of #90):
// GET /games/{slug}'s public fields plus GET /games/{slug}/registry's five
// metrics (#261), each rendered through AvalonMetricTile with its
// definition and class label — never a bare number. `status` renders
// through a visibly distinct badge whenever it isn't "active". Key history
// (#84/#80, both since decided/closed) now reads from GET /games/{slug}/keys.
// "Your access" reuses AvalonConnectionCard/api.revokeGrant/disconnectGame
// exactly as Connections.vue does, scoped to just this game's binding — the
// only part of this view with a session dependency; everything else stays
// public and unauthenticated.
import { computed, ref } from 'vue'
import { useRoute } from 'vue-router'
import { AvalonCard, AvalonConnectionCard, AvalonMetricTile } from '@avalon/ui'
import * as api from '../api/client'
import { isActiveGameStatus } from '../api/games'
import { capabilityDescription } from '../api/connections'
import { useGameProfile } from '../composables/useGameProfile'
import { useMyConnections } from '../composables/useMyConnections'
import { useSessionStore } from '../stores/session'
import gameStyles from './GameProfile.module.scss'
import styles from './page.module.scss'

const route = useRoute()
const slug = computed(() => route.params.slug as string)
const { game, metrics, issuerKeys, loading, error } = useGameProfile(slug)

const session = useSessionStore()
const myConnections = useMyConnections()
const myBinding = computed(() =>
  myConnections.bindings.value.find((b) => b.slug === slug.value),
)
const actionError = ref('')

async function onRevokeGrant(capability: string) {
  if (!session.token) return
  actionError.value = ''
  try {
    await api.revokeGrant(session.token, slug.value, capability)
    await myConnections.refresh()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}

async function onDisconnect() {
  if (!session.token) return
  actionError.value = ''
  try {
    await api.disconnectGame(session.token, slug.value)
    await myConnections.refresh()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}

function formatRegisteredAt(iso: string): string {
  return new Date(iso).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' })
}

function formatKeyDate(iso: string): string {
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
    <p v-if="actionError" :class="styles.error">{{ actionError }}</p>

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

    <AvalonCard title="Issuer key history" subtitle="Every key this issuer has ever registered, and its current status.">
      <p v-if="issuerKeys.length === 0" :class="styles.empty">No keys registered.</p>
      <div v-else :class="gameStyles.keyList">
        <div v-for="key in issuerKeys" :key="key.key_id" :class="gameStyles.keyRow">
          <span :class="gameStyles.keyRole">{{ key.role }}</span>
          <span v-if="key.revoked_at" :class="gameStyles.keyStatus">Revoked {{ formatKeyDate(key.revoked_at) }}</span>
          <span :class="gameStyles.keyDates">Since {{ formatKeyDate(key.valid_from) }}</span>
        </div>
      </div>
    </AvalonCard>

    <AvalonCard v-if="session.isAuthenticated()" title="Your access" subtitle="What this game can see or do with your account.">
      <p v-if="!myBinding" :class="styles.empty">You haven't connected to this game.</p>
      <AvalonConnectionCard
        v-else
        :game-name="myBinding.name"
        :slug="myBinding.slug"
        :established-at="myBinding.established_at"
        :grants="myBinding.grants.map((g) => ({ capability: g.capability, description: capabilityDescription(g.capability) }))"
        @revoke-grant="onRevokeGrant"
        @disconnect="onDisconnect"
      />
    </AvalonCard>
  </div>
</template>
