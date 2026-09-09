<script setup lang="ts">
// Social recovery (issue #201) — the unauthenticated entry point for a
// player who has lost every passkey for an identity and has guardians
// configured. Reachable from Login.vue without a session, same as
// CreateIdentity.vue; unlike that flow, this one drives a WebAuthn
// registration ceremony for a device that ends up owning no valid
// credential until the guardian threshold and time-delay both clear (see
// api/recovery.ts::startRecovery / crates/server/src/recovery.rs).
import { ref } from 'vue'
import { AvalonAuthCard, AvalonButton, AvalonForm, AvalonTextField } from '@avalon/ui'
import { getIdentityRecoveryStatus, startRecovery } from '../api/recovery'
import type { RecoveryRequestResponse } from '../api/types'
import AuthLayout from './AuthLayout.vue'
import styles from './CreateIdentity.module.scss'

const identityId = ref('')
const submitting = ref(false)
const error = ref('')
const request = ref<RecoveryRequestResponse | null>(null)

async function onSubmit() {
  error.value = ''
  submitting.value = true
  try {
    request.value = await startRecovery(identityId.value.trim(), null)
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    submitting.value = false
  }
}

// The status endpoint is itself public (crates/server/src/recovery.rs's
// `identity_recovery_status`) — this "Refresh status" button works even
// though this page has no session, since checking on an in-flight
// recovery is exactly the "public time-delay" invariant the ticket calls
// for.
const refreshing = ref(false)

async function onRefreshStatus() {
  if (!request.value) return
  refreshing.value = true
  try {
    request.value = await getIdentityRecoveryStatus(request.value.identity_id)
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    refreshing.value = false
  }
}
</script>

<template>
  <AuthLayout>
    <AvalonAuthCard
      title="Recover your identity"
      subtitle="Lost every device? If you set up recovery guardians, they can jointly authorize adding this device."
    >
      <AvalonForm
        v-if="!request"
        submit-label="Start recovery"
        :submitting="submitting"
        :error="error"
        @submit="onSubmit"
      >
        <AvalonTextField v-model="identityId" label="Identity id" placeholder="The identity id you're recovering" />
      </AvalonForm>

      <div v-else :class="styles.stack">
        <p :class="styles.hint">
          A recovery request has been created for this device. Your guardians need to approve it
          — once enough of them have, there's a mandatory public delay before this device becomes
          usable, so the real owner has time to cancel it if this wasn't them.
        </p>
        <p><strong>Status:</strong> {{ request.status }}</p>
        <p><strong>Approvals:</strong> {{ request.approvals_count }} of {{ request.threshold }}</p>
        <p v-if="request.delay_ends_at"><strong>Delay ends:</strong> {{ request.delay_ends_at }}</p>
        <p v-if="error" :class="styles.hint">{{ error }}</p>
        <div :class="styles.actions">
          <AvalonButton
            :label="refreshing ? 'Checking…' : 'Refresh status'"
            variant="primary"
            :disabled="refreshing"
            @click="onRefreshStatus"
          />
        </div>
      </div>

      <p :class="styles.switchLink">
        <RouterLink to="/login">Back to log in</RouterLink>
      </p>
    </AvalonAuthCard>
  </AuthLayout>
</template>
