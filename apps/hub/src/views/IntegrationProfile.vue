<script setup lang="ts">
// Per-integrator profile page (issue #270, first buildable slice of #90):
// GET /integrations/{slug}'s public fields plus GET /integrations/{slug}/registry's five
// metrics (#261), each rendered through AvalonMetricTile with its
// definition and class label — never a bare number. `status` renders
// through a visibly distinct badge whenever it isn't "active". Key history
// (#84/#80, both since decided/closed) now reads from GET /integrations/{slug}/keys.
// "Your access" reuses AvalonConnectionCard/api.revokeGrant/disconnectIntegrator
// exactly as Connections.vue does, scoped to just this integrator's binding — the
// only part of this view with a session dependency; everything else stays
// public and unauthenticated.
import { computed, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { AvalonButton, AvalonCard, AvalonConnectionCard, AvalonGuildCard, AvalonMetricTile } from '@avalon/ui'
import { isActiveIntegratorStatus } from '../api/integrations'
import { capabilityDescription } from '../api/connections'
import { buildDiscoverQueryString } from '../api/guilds'
import type { DiscoverGuildSummary } from '@avalon/sdk'
import { useIntegrationProfile } from '../composables/useIntegrationProfile'
import { useMyConnections } from '../composables/useMyConnections'
import { useSessionStore } from '../api/session'
import integratorStyles from '../styles/IntegrationProfile.module.scss'
import styles from '../styles/page.module.scss'

const route = useRoute()
const router = useRouter()
const slug = computed(() => route.params.slug as string)

function onConnect() {
  router.push({ name: 'connect-integrator', params: { slug: slug.value } })
}
const { integrator, metrics, issuerKeys, loading, error } = useIntegrationProfile(slug)

const session = useSessionStore()
const myConnections = useMyConnections()
const myBinding = computed(() =>
  myConnections.bindings.value.find((b) => b.slug === slug.value),
)
const actionError = ref('')

async function onRevokeGrant(capability: string) {
  const s = session.session
  if (!s) return
  actionError.value = ''
  try {
    await s.revokeGrant(slug.value, capability)
    await myConnections.refresh()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}

async function onDisconnect() {
  const s = session.session
  if (!s) return
  actionError.value = ''
  try {
    await s.disconnectIntegrator(slug.value)
    await myConnections.refresh()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}

function formatRegisteredAt(iso: string): string {
  return new Date(iso).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' })
}

// Issue #467 — "Guilds playing this": a small preview (GET /guilds/discover
// is session-authenticated, same gate Guilds.vue's own Discover tab
// already has) linking through to that tab pre-filtered to the same
// integrator, rather than duplicating full discovery UI on this page.
const GUILDS_PLAYING_PREVIEW_LIMIT = 5
const guildsPlaying = ref<DiscoverGuildSummary[]>([])
const guildsPlayingError = ref('')

async function refreshGuildsPlaying() {
  const s = session.session
  if (!s) return
  try {
    const response = await s.discoverGuilds(
      buildDiscoverQueryString({ integrator: slug.value, limit: GUILDS_PLAYING_PREVIEW_LIMIT }),
    )
    if (Array.isArray(response.guilds)) guildsPlaying.value = response.guilds
  } catch (e) {
    guildsPlayingError.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}
watch(slug, refreshGuildsPlaying, { immediate: true })

function onSeeAllGuildsPlaying() {
  router.push({ name: 'guilds', query: { integrator: slug.value } })
}

function formatKeyDate(iso: string): string {
  return new Date(iso).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' })
}
</script>

<template>
  <div v-if="!loading && integrator" :class="styles.page">
    <header :class="styles.pageHeader">
      <div :class="integratorStyles.heading">
        <h1 :class="styles.title">{{ integrator.name }}</h1>
        <span v-if="!isActiveIntegratorStatus(integrator.status)" :class="integratorStyles.statusBadge">{{ integrator.status }}</span>
      </div>
      <p :class="styles.subtitle">{{ integrator.ownerName }} · Registered {{ formatRegisteredAt(integrator.registeredAt) }}</p>
    </header>

    <p v-if="error" :class="styles.error">{{ error }}</p>
    <p v-if="actionError" :class="styles.error">{{ actionError }}</p>

    <AvalonCard title="Registry metrics" subtitle="Facts derived from protocol activity, shown exactly as the registry reports them.">
      <div :class="integratorStyles.metricsGrid">
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
      <div v-else :class="integratorStyles.keyList">
        <div v-for="key in issuerKeys" :key="key.keyId" :class="integratorStyles.keyRow">
          <span :class="integratorStyles.keyRole">{{ key.role }}</span>
          <span v-if="key.revokedAt" :class="integratorStyles.keyStatus">Revoked {{ formatKeyDate(key.revokedAt) }}</span>
          <span :class="integratorStyles.keyDates">Since {{ formatKeyDate(key.validFrom) }}</span>
        </div>
      </div>
    </AvalonCard>

    <AvalonCard
      v-if="session.isAuthenticated()"
      title="Guilds playing this"
      subtitle="Guilds associated with this integrator, recruiting or not."
    >
      <p v-if="guildsPlayingError" :class="styles.error">{{ guildsPlayingError }}</p>
      <p v-else-if="guildsPlaying.length === 0" :class="styles.empty">No guilds found for this integrator yet.</p>
      <AvalonGuildCard
        v-for="guild in guildsPlaying"
        :key="guild.id"
        :name="guild.name"
        :tag="guild.tag"
        :description="guild.description"
        :member-count="guild.memberCount"
        :recruiting="guild.recruiting"
        :icon-url="guild.icon ?? undefined"
        :banner-url="guild.banner ?? undefined"
        @select="router.push({ name: 'guild', params: { id: guild.id } })"
      />
      <AvalonButton label="See all in Discover" variant="secondary" @click="onSeeAllGuildsPlaying" />
    </AvalonCard>

    <AvalonCard v-if="session.isAuthenticated()" title="Your access" subtitle="What this integrator can see or do with your account.">
      <template v-if="!myBinding">
        <p :class="styles.empty">You haven't connected to this integrator.</p>
        <AvalonButton label="Connect" variant="primary" @click="onConnect" />
      </template>
      <AvalonConnectionCard
        v-else
        :integrator-name="myBinding.name"
        :slug="myBinding.slug"
        :established-at="myBinding.establishedAt"
        :grants="myBinding.grants.map((g) => ({ capability: g.capability, description: capabilityDescription(g.capability) }))"
        @revoke-grant="onRevokeGrant"
        @disconnect="onDisconnect"
      />
    </AvalonCard>
  </div>
</template>
