<script setup lang="ts">
// Connected-integrators view: every IntegratorBinding the caller has, with
// its currently-active grants, a per-grant revoke button, and a disconnect
// button. GET /me/connections is the source of truth — no client-side
// merging needed, unlike the guild roster's presence merge.
import { ref } from 'vue'
import { AvalonCard, AvalonConnectionCard } from '@avalon-initiative/common-ui'
import { capabilityDescription } from '../api/connections'
import { useMyConnections } from '../composables/useMyConnections'
import { useSessionStore } from '../api/session'
import styles from '../styles/page.module.scss'

const session = useSessionStore()
const { bindings, loading, error, refresh } = useMyConnections()

const actionError = ref('')

async function onRevokeGrant(slug: string, capability: string) {
  const s = session.session
  if (!s) return
  actionError.value = ''
  try {
    await s.revokeGrant(slug, capability)
    await refresh()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}

async function onDisconnect(slug: string) {
  const s = session.session
  if (!s) return
  actionError.value = ''
  try {
    await s.disconnectIntegrator(slug)
    await refresh()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}
</script>

<template>
  <div v-if="!loading" :class="styles.page">
    <header :class="styles.pageHeader">
      <h1 :class="styles.title">Connected integrators</h1>
      <p :class="styles.subtitle">Integrators you've granted access to, and what they can see or do.</p>
    </header>

    <p v-if="error" :class="styles.error">{{ error }}</p>
    <p v-if="actionError" :class="styles.error">{{ actionError }}</p>

    <AvalonCard title="Connections">
      <p v-if="bindings.length === 0" :class="styles.empty">
        You haven't connected to any integrators yet.
      </p>
      <AvalonConnectionCard
        v-for="binding in bindings"
        :key="binding.bindingId"
        :integrator-name="binding.name"
        :slug="binding.slug"
        :established-at="binding.establishedAt"
        :grants="binding.grants.map((g) => ({ capability: g.capability, description: capabilityDescription(g.capability) }))"
        @revoke-grant="onRevokeGrant(binding.slug, $event)"
        @disconnect="onDisconnect(binding.slug)"
      />
    </AvalonCard>
  </div>
</template>
