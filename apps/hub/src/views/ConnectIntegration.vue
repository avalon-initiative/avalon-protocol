<script setup lang="ts">
// Consent view for connecting to an integrator: shows the integrator's name and
// owner plus every requested capability with a plain-language
// description and an unchecked-by-default checkbox — no "approve all".
// Submitting posts only the checked subset to POST /integrations/{slug}/connect,
// which is also where the IntegratorBinding gets established.
import { computed, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { AvalonButton, AvalonCapabilityConsentRow, AvalonCard, AvalonForm } from '@avalon-initiative/common-ui'
import { capabilityDescription } from '../api/connections'
import { useIntegrationConsent } from '../composables/useIntegrationConsent'
import { useSessionStore } from '../api/session'
import styles from '../styles/page.module.scss'

const route = useRoute()
const router = useRouter()
const session = useSessionStore()

const slug = computed(() => route.params.slug as string)
const { integrator, loading, error, checkedCapabilities, setCapabilityChecked, load } = useIntegrationConsent(slug)

load()

const connecting = ref(false)
const connectError = ref('')

async function onConnect() {
  const s = session.session
  if (!s || !integrator.value) return
  connectError.value = ''
  connecting.value = true
  try {
    // #697/#698: hands a third party standing permission over the
    // identity's data going forward — signature-required, signs
    // automatically inside AccountSession.connectIntegrator.
    const capabilities = Array.from(checkedCapabilities.value)
    await s.connectIntegrator(integrator.value.slug, capabilities)
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
      <p :class="styles.subtitle">{{ integrator.ownerName }}</p>
    </header>

    <p v-if="error" :class="styles.error">{{ error }}</p>

    <AvalonCard title="Requested access">
      <p v-if="integrator.requestedCapabilities.length === 0" :class="styles.empty">
        This integrator hasn't requested any capabilities.
      </p>
      <AvalonCapabilityConsentRow
        v-for="capability in integrator.requestedCapabilities"
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
