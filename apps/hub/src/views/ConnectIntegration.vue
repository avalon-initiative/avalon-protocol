<script setup lang="ts">
// Consent view for connecting to an integrator (#27): shows the integrator's name and
// owner plus every requested capability with a plain-language
// description and an unchecked-by-default checkbox — no "approve all".
// Submitting posts only the checked subset to POST /integrations/{slug}/connect,
// which is also where the IntegratorBinding (#83) gets established.
import { computed, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { AvalonButton, AvalonCapabilityConsentRow, AvalonCard, AvalonForm } from '@avalon/ui'
import * as api from '../api/client'
import { capabilityDescription } from '../api/connections'
import { useIntegrationConsent } from '../composables/useIntegrationConsent'
import { useSessionStore } from '../stores/session'
import styles from './page.module.scss'

const route = useRoute()
const router = useRouter()
const session = useSessionStore()

const slug = computed(() => route.params.slug as string)
const { integrator, loading, error, checkedCapabilities, setCapabilityChecked, load } = useIntegrationConsent(slug)

load()

const connecting = ref(false)
const connectError = ref('')

async function onConnect() {
  if (!session.token || !integrator.value) return
  connectError.value = ''
  connecting.value = true
  try {
    await api.connectIntegrator(session.token, integrator.value.slug, {
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
  <div v-if="!loading && integrator" :class="styles.page">
    <header :class="styles.pageHeader">
      <h1 :class="styles.title">Connect to {{ integrator.name }}</h1>
      <p :class="styles.subtitle">{{ integrator.owner_name }}</p>
    </header>

    <p v-if="error" :class="styles.error">{{ error }}</p>

    <AvalonCard title="Requested access">
      <p v-if="integrator.requested_capabilities.length === 0" :class="styles.empty">
        This integrator hasn't requested any capabilities.
      </p>
      <AvalonCapabilityConsentRow
        v-for="capability in integrator.requested_capabilities"
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
