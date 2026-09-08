<script setup lang="ts">
// Connected-games view (#27, #83): every GameBinding the caller has, with
// its currently-active grants, a per-grant revoke button, and a disconnect
// button. GET /me/connections is the source of truth — no client-side
// merging needed, unlike the guild roster (issue #24's presence merge).
import { ref } from 'vue'
import { AvalonCard, AvalonConnectionCard } from '@avalon/ui'
import * as api from '../api/client'
import { capabilityDescription } from '../api/connections'
import { useMyConnections } from '../composables/useMyConnections'
import { useSessionStore } from '../stores/session'
import styles from './page.module.scss'

const session = useSessionStore()
const { bindings, loading, error, refresh } = useMyConnections()

const actionError = ref('')

async function onRevokeGrant(slug: string, capability: string) {
  if (!session.token) return
  actionError.value = ''
  try {
    await api.revokeGrant(session.token, slug, capability)
    await refresh()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}

async function onDisconnect(slug: string) {
  if (!session.token) return
  actionError.value = ''
  try {
    await api.disconnectGame(session.token, slug)
    await refresh()
  } catch (e) {
    actionError.value = e instanceof Error ? e.message : 'Something went wrong.'
  }
}
</script>

<template>
  <div v-if="!loading" :class="styles.page">
    <header :class="styles.pageHeader">
      <h1 :class="styles.title">Connected games</h1>
      <p :class="styles.subtitle">Games you've granted access to, and what they can see or do.</p>
    </header>

    <p v-if="error" :class="styles.error">{{ error }}</p>
    <p v-if="actionError" :class="styles.error">{{ actionError }}</p>

    <AvalonCard title="Connections">
      <p v-if="bindings.length === 0" :class="styles.empty">
        You haven't connected to any games yet.
      </p>
      <AvalonConnectionCard
        v-for="binding in bindings"
        :key="binding.binding_id"
        :game-name="binding.name"
        :slug="binding.slug"
        :established-at="binding.established_at"
        :grants="binding.grants.map((g) => ({ capability: g.capability, description: capabilityDescription(g.capability) }))"
        @revoke-grant="onRevokeGrant(binding.slug, $event)"
        @disconnect="onDisconnect(binding.slug)"
      />
    </AvalonCard>
  </div>
</template>
