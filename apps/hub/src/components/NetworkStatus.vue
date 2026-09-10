<script setup lang="ts">
// Issue #232: the always-visible "which network am I actually talking to"
// indicator — replaces HubShell's old static "Connected to Avalon" text.
// Convention: no <style> blocks, styling lives in the co-located
// .module.scss; this file's script stays glue over useNetworkTrust plus one
// tiny local ref for the details toggle.
import { ref } from 'vue'
import { useNetworkTrust } from '../composables/useNetworkTrust'
import { getServerUrl, setServerUrl } from '../api/client'
import { AvalonModal } from '@avalon/ui'
import styles from './NetworkStatus.module.scss'

const { state, knownNetworks, refresh } = useNetworkTrust()
const expanded = ref(false)
const customUrl = ref('')
const showCustomForm = ref(false)

// Switching networks means every bearer token/session state currently held
// was established against the *old* server — there's no meaningful way to
// carry it over, so a full reload is the honest, simple choice (the
// existing session store already treats a missing/invalid token as
// logged-out, so this degrades to "please sign in again" rather than
// anything silently broken).
function switchTo(url: string) {
  setServerUrl(url)
  window.location.reload()
}

function connectToCustomUrl() {
  const trimmed = customUrl.value.trim()
  if (!trimmed) return
  switchTo(trimmed)
}

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
      @click="expanded = true"
    >
      <span :class="styles.dot" />
      <span :class="styles.label">{{ statusLabel() }}</span>
    </button>

    <AvalonModal title="Network trust" :open="expanded" @close="expanded = false">
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

      <section :class="styles.section">
        <h3 :class="styles.sectionHeading">Networks this Hub build recognizes</h3>
        <ul :class="styles.knownList">
          <li v-for="network in knownNetworks" :key="network.network_id" :class="styles.knownItem">
            <span :class="styles.knownLabel">{{ network.label }}</span>
            <span :class="styles.knownId">{{ network.network_id }}</span>
            <span
              v-if="network.environment !== 'prod'"
              :class="[styles.knownEnvironment, styles[network.environment]]"
              >{{ network.environment }}</span
            >
          </li>
        </ul>
        <button type="button" :class="styles.refresh" @click="refresh">Re-check now</button>
      </section>

      <!-- Issue #232's actual network selector — picking an entry (or a
           custom URL) is an explicit, visible switch, never silent, and the
           active network is always what the status line above reports
           after the reload this triggers. -->
      <section :class="styles.section">
        <h3 :class="styles.sectionHeading">Switch network</h3>
        <ul :class="styles.switchList">
          <li v-for="network in knownNetworks" :key="network.network_id" :class="styles.switchItem">
            <span>{{ network.label }}</span>
            <span v-if="network.server_url === getServerUrl()" :class="styles.currentBadge"
              >Current</span
            >
            <button
              v-else-if="network.server_url"
              type="button"
              :class="styles.switchButton"
              @click="switchTo(network.server_url)"
            >
              Switch
            </button>
            <span v-else :class="styles.knownId">No known server URL yet</span>
          </li>
        </ul>

        <button
          v-if="!showCustomForm"
          type="button"
          :class="styles.switchButton"
          @click="showCustomForm = true"
        >
          Connect to a custom network…
        </button>
        <div v-else :class="styles.customForm">
          <input
            v-model="customUrl"
            type="text"
            placeholder="https://…"
            :class="styles.customInput"
            @keyup.enter="connectToCustomUrl"
          />
          <button type="button" :class="styles.switchButton" @click="connectToCustomUrl">
            Connect
          </button>
        </div>
        <p v-if="showCustomForm" :class="[styles.detail, styles.customWarning]">
          A custom network is never treated as verified unless its Signed Tree Head happens to
          match an entry already pinned in this Hub build's trust-anchor list.
        </p>
      </section>
    </AvalonModal>
  </div>
</template>
