<script setup lang="ts">
// Issue #232: the always-visible "which network am I actually talking to"
// indicator — replaces HubShell's old static "Connected to Avalon" text.
// Convention: no <style> blocks, styling lives in the co-located
// .module.scss; this file's script stays glue over useNetworkTrust plus one
// tiny local ref for the details toggle.
import { ref } from 'vue'
import { useNetworkTrust } from '../composables/useNetworkTrust'
import styles from './NetworkStatus.module.scss'

const { state, knownNetworks, refresh } = useNetworkTrust()
const expanded = ref(false)

function statusLabel(): string {
  switch (state.value.kind) {
    case 'loading':
      return 'Checking network…'
    case 'unreachable':
      return 'Network unreachable'
    case 'verified':
      return state.value.entry.label
    case 'mismatch':
      return `Key mismatch: ${state.value.claimedNetworkId}`
    case 'unknown-network':
      return `Unverified: ${state.value.claimedNetworkId}`
  }
}

function statusTone(): 'ok' | 'warn' | 'danger' | 'pending' {
  switch (state.value.kind) {
    case 'loading':
      return 'pending'
    case 'verified':
      return 'ok'
    case 'unreachable':
    case 'unknown-network':
      return 'warn'
    case 'mismatch':
      return 'danger'
  }
}
</script>

<template>
  <div :class="styles.networkStatus">
    <button
      type="button"
      :class="[styles.summary, styles[statusTone()]]"
      :aria-expanded="expanded"
      @click="expanded = !expanded"
    >
      <span :class="styles.dot" />
      <span :class="styles.label">{{ statusLabel() }}</span>
    </button>

    <div v-if="expanded" :class="styles.panel">
      <p v-if="state.kind === 'verified'" :class="styles.detail">
        Signed Tree Head verified against the pinned key for
        <code>{{ state.entry.network_id }}</code>.
      </p>
      <p v-else-if="state.kind === 'mismatch'" :class="styles.detailDanger">
        This server claims <code>{{ state.claimedNetworkId }}</code>, but its Signed Tree Head does
        not verify against the key pinned for that network — this may not be the real network.
      </p>
      <p v-else-if="state.kind === 'unknown-network'" :class="styles.detail">
        This server claims <code>{{ state.claimedNetworkId }}</code>, which isn't in this Hub
        build's pinned trust-anchor list — treated as unverified, not trusted.
      </p>
      <p v-else-if="state.kind === 'unreachable'" :class="styles.detail">
        {{ state.message }}
      </p>

      <p :class="styles.knownHeading">Networks this Hub build recognizes:</p>
      <ul :class="styles.knownList">
        <li v-for="network in knownNetworks" :key="network.network_id" :class="styles.knownItem">
          <span :class="styles.knownLabel">{{ network.label }}</span>
          <span :class="styles.knownId">{{ network.network_id }}</span>
          <span v-if="network.placeholder" :class="styles.knownPlaceholder">placeholder</span>
        </li>
      </ul>

      <button type="button" :class="styles.refresh" @click="refresh">Re-check now</button>
    </div>
  </div>
</template>
