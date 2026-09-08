<script setup lang="ts">
// Consent view for connecting to a game (#27): shows the game's name and
// developer plus every requested capability with a plain-language
// description and an unchecked-by-default checkbox — no "approve all".
// Submitting posts only the checked subset to POST /games/{slug}/connect,
// which is also where the GameBinding (#83) gets established.
import { computed, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { AvalonButton, AvalonCapabilityConsentRow, AvalonCard, AvalonForm } from '@avalon/ui'
import * as api from '../api/client'
import { capabilityDescription } from '../api/connections'
import { useGameConsent } from '../composables/useGameConsent'
import { useSessionStore } from '../stores/session'
import styles from './page.module.scss'

const route = useRoute()
const router = useRouter()
const session = useSessionStore()

const slug = computed(() => route.params.slug as string)
const { game, loading, error, checkedCapabilities, setCapabilityChecked, load } = useGameConsent(slug)

load()

const connecting = ref(false)
const connectError = ref('')

async function onConnect() {
  if (!session.token || !game.value) return
  connectError.value = ''
  connecting.value = true
  try {
    await api.connectGame(session.token, game.value.slug, {
      capabilities: Array.from(checkedCapabilities.value),
    })
    router.push({ name: 'connections' })
  } catch (e) {
    connectError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    connecting.value = false
  }
}

function onCancel() {
  router.push({ name: 'connections' })
}
</script>

<template>
  <div v-if="!loading && game" :class="styles.page">
    <header :class="styles.pageHeader">
      <h1 :class="styles.title">Connect to {{ game.name }}</h1>
      <p :class="styles.subtitle">{{ game.developer }}</p>
    </header>

    <p v-if="error" :class="styles.error">{{ error }}</p>

    <AvalonCard title="Requested access">
      <p v-if="game.requested_capabilities.length === 0" :class="styles.empty">
        This game hasn't requested any capabilities.
      </p>
      <AvalonCapabilityConsentRow
        v-for="capability in game.requested_capabilities"
        :key="capability"
        :capability="capability"
        :description="capabilityDescription(capability)"
        :checked="checkedCapabilities.has(capability)"
        @update:checked="setCapabilityChecked(capability, $event)"
      />
      <AvalonForm
        submit-label="Connect"
        :submitting="connecting"
        :error="connectError"
        @submit="onConnect"
      >
        <template #secondary-actions>
          <AvalonButton label="Cancel" variant="secondary" @click="onCancel" />
        </template>
      </AvalonForm>
    </AvalonCard>
  </div>
</template>
