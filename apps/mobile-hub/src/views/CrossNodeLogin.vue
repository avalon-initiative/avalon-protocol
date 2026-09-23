<script setup lang="ts">
// Cross-node login approval — mobile-hub's own
// entry point into the same approval logic
// `apps/hub/src/views/CrossNodeLogin.vue` already established:
// `lookupCrossNodeLogin` before rendering anything approvable (the
// decided phishing-context requirement), then a locally signed grant
// submitted directly to the *requesting* node — never a bearer token sent
// anywhere, never this app's own configured server unless that happens to
// be the requesting node too. Neither app imports the other's `.vue` file
// (they're separate Tauri/Vite apps); the actual shared logic lives in
// `@avalon/api-client`, imported identically by both.
//
// #642's own no-auto-approve requirement, specifically owned by this
// screen: a deep link never resolves straight to an approved session on
// its own — landing here always requires an explicit tap on "Approve"
// after the request's real context has been shown, whether this screen
// was reached by typing a code or by a deep link/QR scan prefilling it.
import { ref } from 'vue'
import { useRoute } from 'vue-router'
import { AvalonButton, AvalonCard, AvalonIcon, AvalonTextField, AvalonWarningBanner } from '@avalon/ui'
import * as api from '@avalon/api-client'
import { useSessionStore } from '@avalon/api-client'
import { loadSigningKey, mintCrossNodeLoginGrant } from '@avalon/api-client'
import styles from '../styles/CrossNodeLogin.module.scss'

const route = useRoute()
const session = useSessionStore()

const nodeBaseUrl = ref(typeof route.query.node === 'string' ? route.query.node : '')
const userCode = ref(typeof route.query.user_code === 'string' ? route.query.user_code : '')
const requestingContext = ref('')
// Epic #623 issue #649: whether the requesting node resolves to a real,
// registered integrator (or a known network anchor) — #642's decided
// visual distinction, never a hard gate against an unverified requester.
const integratorVerified = ref(false)
const displayName = ref('')
const lookupStatus = ref<'pending' | 'denied' | 'expired' | 'approved' | null>(null)
const submitting = ref(false)
const looking = ref(false)
const error = ref('')
const resolvedStatus = ref<'approved' | 'denied' | null>(null)

async function onLookUp() {
  const node = nodeBaseUrl.value.trim().replace(/\/$/, '')
  const code = userCode.value.trim()
  if (!node || !code) return
  error.value = ''
  looking.value = true
  lookupStatus.value = null
  try {
    const result = await api.lookupCrossNodeLogin(node, code)
    lookupStatus.value = result.status
    requestingContext.value = result.requesting_context
    integratorVerified.value = result.integrator_verified
    displayName.value = result.display_name ?? ''
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    looking.value = false
  }
}

async function onApprove() {
  const node = nodeBaseUrl.value.trim().replace(/\/$/, '')
  const code = userCode.value.trim()
  if (!node || !code || !session.identityId || !session.signingKeyId) return

  const secretKey = loadSigningKey(session.identityId)
  if (!secretKey) {
    error.value =
      "This device doesn't have your signing key — recover it from your recovery phrase first, then try again."
    return
  }

  error.value = ''
  submitting.value = true
  try {
    const grant = mintCrossNodeLoginGrant(
      session.identityId,
      session.signingKeyId,
      secretKey,
      node,
      requestingContext.value,
    )
    await api.submitCrossNodeLoginGrant(node, { user_code: code, grant })
    resolvedStatus.value = 'approved'
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    submitting.value = false
  }
}

async function onDeny() {
  const node = nodeBaseUrl.value.trim().replace(/\/$/, '')
  const code = userCode.value.trim()
  if (!node || !code) return
  error.value = ''
  submitting.value = true
  try {
    await api.denyCrossNodeLogin(node, { user_code: code })
    resolvedStatus.value = 'denied'
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    submitting.value = false
  }
}

function onStartOver() {
  resolvedStatus.value = null
  lookupStatus.value = null
  userCode.value = ''
  requestingContext.value = ''
  integratorVerified.value = false
  displayName.value = ''
  error.value = ''
}
</script>

<template>
  <main :class="styles.main">
    <AvalonCard title="Sign in on another node">
      <template v-if="resolvedStatus === 'approved'">
        <p>Approved. That device should sign in automatically within a few seconds.</p>
        <AvalonButton label="Approve another request" variant="secondary" @click="onStartOver" />
      </template>
      <template v-else-if="resolvedStatus === 'denied'">
        <p>Request denied.</p>
        <AvalonButton label="Approve another request" variant="secondary" @click="onStartOver" />
      </template>
      <template v-else-if="lookupStatus === 'pending'">
        <p v-if="error" :class="styles.error">{{ error }}</p>
        <div v-if="integratorVerified" :class="[styles.context, styles.verified]">
          <AvalonIcon name="shield" :class="styles.verifiedIcon" />
          <div>
            <span :class="styles.contextLabel">Verified requester</span>
            <span :class="styles.contextValue">{{ displayName }}</span>
          </div>
        </div>
        <AvalonWarningBanner
          v-else
          title="Unverified requester"
          :message="`${requestingContext} is not a registered integrator on this network. Only approve if you recognize it.`"
        />
        <div :class="styles.context">
          <span :class="styles.contextLabel">Requesting node</span>
          <span :class="styles.contextValue">{{ requestingContext }}</span>
        </div>
        <div :class="styles.actions">
          <AvalonButton
            label="Approve"
            variant="primary"
            :disabled="submitting"
            @click="onApprove"
          />
          <AvalonButton label="Deny" variant="danger" :disabled="submitting" @click="onDeny" />
        </div>
      </template>
      <template v-else>
        <p v-if="lookupStatus" :class="styles.notPending">
          This request has already been {{ lookupStatus }} — nothing to approve.
        </p>
        <p v-if="error" :class="styles.error">{{ error }}</p>
        <AvalonTextField
          v-model="nodeBaseUrl"
          label="Requesting node's address"
          placeholder="https://node-b.example"
          :disabled="looking"
        />
        <AvalonTextField
          v-model="userCode"
          label="Code"
          placeholder="ABCD2345"
          :disabled="looking"
        />
        <div :class="styles.actions">
          <AvalonButton
            label="Look up"
            variant="primary"
            :disabled="looking || !nodeBaseUrl.trim() || !userCode.trim()"
            @click="onLookUp"
          />
        </div>
      </template>
    </AvalonCard>
  </main>
</template>
