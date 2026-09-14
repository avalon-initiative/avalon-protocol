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
import { computed, ref } from 'vue'
import { useRoute } from 'vue-router'
import { AvalonCard, AvalonConnectionCard, AvalonMetricTile } from '@avalon/ui'
import * as api from '../api/client'
import { isActiveIntegratorStatus } from '../api/integrations'
import { capabilityDescription } from '../api/connections'
import { useIntegrationProfile } from '../composables/useIntegrationProfile'
import { useMyConnections } from '../composables/useMyConnections'
import { useSessionStore } from '../stores/session'
import integratorStyles from './IntegrationProfile.module.scss'
import styles from './page.module.scss'

const route = useRoute()
const slug = computed(() => route.params.slug as string)
const { integrator, metrics, issuerKeys, loading, error } = useIntegrationProfile(slug)

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
    await api.disconnectIntegrator(session.token, slug.value)
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
  <div v-if="!loading && integrator" :class="styles.page">
    <header :class="styles.pageHeader">
      <div :class="integratorStyles.heading">
        <h1 :class="styles.title">{{ integrator.name }}</h1>
        <span v-if="!isActiveIntegratorStatus(integrator.status)" :class="integratorStyles.statusBadge">{{ integrator.status }}</span>
      </div>
      <p :class="styles.subtitle">{{ integrator.owner_name }} · Registered {{ formatRegisteredAt(integrator.registered_at) }}</p>
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
        <div v-for="key in issuerKeys" :key="key.key_id" :class="integratorStyles.keyRow">
          <span :class="integratorStyles.keyRole">{{ key.role }}</span>
          <span v-if="key.revoked_at" :class="integratorStyles.keyStatus">Revoked {{ formatKeyDate(key.revoked_at) }}</span>
          <span :class="integratorStyles.keyDates">Since {{ formatKeyDate(key.valid_from) }}</span>
        </div>
      </div>
    </AvalonCard>

    <AvalonCard v-if="session.isAuthenticated()" title="Your access" subtitle="What this integrator can see or do with your account.">
      <p v-if="!myBinding" :class="styles.empty">You haven't connected to this integrator.</p>
      <AvalonConnectionCard
        v-else
        :integrator-name="myBinding.name"
        :slug="myBinding.slug"
        :established-at="myBinding.established_at"
        :grants="myBinding.grants.map((g) => ({ capability: g.capability, description: capabilityDescription(g.capability) }))"
        @revoke-grant="onRevokeGrant"
        @disconnect="onDisconnect"
      />
    </AvalonCard>
  </div>
</template>
