<script setup lang="ts">
// Social recovery — the unauthenticated entry point for a
// user who has lost every passkey for an identity and has guardians
// configured. Reachable from Login.vue without a session, same as
// CreateIdentity.vue; unlike that flow, this one drives a WebAuthn
// registration ceremony for a device that ends up owning no valid
// credential until the guardian threshold and time-delay both clear (see
// api/recovery.ts::startRecovery / crates/server/src/recovery.rs).
import { computed, ref } from 'vue'
import { useRouter } from 'vue-router'
import { AvalonAuthCard, AvalonButton, AvalonForm, AvalonTextField } from '@avalon/ui'
import type { RecoveryRequest } from '@avalon-initiative/protocol-sdk'
import { avalonClient, useSessionStore } from '../api/session'
import { loadSigningKeySeed } from '../api/signingKeyStorage'
import { finalizeRecoveryRequest, getIdentityRecoveryStatus, startRecovery } from '../api/recovery'
import AuthLayout from './AuthLayout.vue'
import styles from '../styles/CreateIdentity.module.scss'

const router = useRouter()
const session = useSessionStore()

const identityId = ref('')
const submitting = ref(false)
const error = ref('')
const request = ref<RecoveryRequest | null>(null)

// Mirrors the server's own `guard_can_finalize` (crates/server/src/recovery.rs):
// only a `delay`-status request whose delay has actually elapsed may finalize.
// This is purely a UI gate — the server re-checks the same condition and
// `onFinalize` surfaces whatever it says either way.
const canFinalize = computed(() => {
  if (!request.value || request.value.status !== 'delay' || !request.value.delayEndsAt) return false
  return new Date(request.value.delayEndsAt).getTime() <= Date.now()
})

async function onSubmit() {
  error.value = ''
  submitting.value = true
  try {
    request.value = await startRecovery(identityId.value.trim())
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
    request.value = await getIdentityRecoveryStatus(request.value.identityId)
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    refreshing.value = false
  }
}

// Completes recovery once guardians have approved and the delay has
// elapsed: finalizing on the server activates this device's passkey
// credential, then a normal `login` (WebAuthn assertion ceremony) signs it
// in exactly the way any other device does.
const finalizing = ref(false)

async function onFinalize() {
  if (!request.value) return
  error.value = ''
  finalizing.value = true
  try {
    const identityId = request.value.identityId
    request.value = await finalizeRecoveryRequest(request.value.id)
    const accountSession = await avalonClient().loginWithIdentityId(identityId)
    // Guardian-based recovery only ever registers a new
    // WebAuthn passkey, never a new Ed25519 signing key (startRecovery
    // above), so loadSigningKeySeed correctly returns null here until this
    // device separately goes through #135's device-grant flow — no
    // reconnect-across-nodes support for this session until then, same as
    // any other signing-key-less device.
    const secretKey = loadSigningKeySeed(identityId)
    if (secretKey) await accountSession.attachSigningKey(secretKey)
    session.setSession(accountSession)
    await router.push({ name: 'home' })
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    finalizing.value = false
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
        <p><strong>Approvals:</strong> {{ request.approvalsCount }} of {{ request.threshold }}</p>
        <p v-if="request.delayEndsAt"><strong>Delay ends:</strong> {{ request.delayEndsAt }}</p>
        <p v-if="error" :class="styles.hint">{{ error }}</p>
        <div :class="styles.actions">
          <AvalonButton
            v-if="canFinalize"
            :label="finalizing ? 'Finishing…' : 'Finish recovery'"
            variant="primary"
            :disabled="finalizing"
            @click="onFinalize"
          />
          <AvalonButton
            :label="refreshing ? 'Checking…' : 'Refresh status'"
            variant="secondary"
            :disabled="refreshing || finalizing"
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
