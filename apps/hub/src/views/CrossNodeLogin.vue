<script setup lang="ts">
// Cross-node login approval: lets the user approve
// logging their identity into a node other than this Hub's own — the
// requesting device (a game, a console, a browser that's never talked to
// this identity before) started the request there, not here. Unlike
// PairDevice.vue's same-node pairing, this Hub has to talk to an arbitrary
// requesting node's own base_url, and the approval itself is a locally
// signed grant (`mintCrossNodeLoginGrant`), not a session-authenticated
// approve call — this Hub's own session only ever supplies the identity's
// signing key material, never a bearer token sent to the requesting node.
//
// #642 (decided): always show real context before rendering an
// approve/deny choice — `lookupCrossNodeLogin` runs before anything else,
// and the requesting node/context is shown plainly, never hidden behind a
// single-tap default.
import { ref } from 'vue'
import { useRoute } from 'vue-router'
import { AvalonButton, AvalonCard, AvalonIcon, AvalonTextField, AvalonWarningBanner } from '@avalon-initiative/common-ui'
import { lookupCrossNodeLogin, denyCrossNodeLogin, submitCrossNodeLoginGrant } from '@avalon-initiative/protocol-sdk'
import { useSessionStore } from '../api/session'
import { loadSigningKeySeed } from '../api/signingKeyStorage'
import styles from '../styles/page.module.scss'
import local from '../styles/CrossNodeLogin.module.scss'

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
    const result = await lookupCrossNodeLogin(node, code)
    lookupStatus.value = result.status
    requestingContext.value = result.requestingContext
    integratorVerified.value = result.integratorVerified
    displayName.value = result.displayName ?? ''
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    looking.value = false
  }
}

async function onApprove() {
  const node = nodeBaseUrl.value.trim().replace(/\/$/, '')
  const code = userCode.value.trim()
  if (!node || !code || !session.identityId() || !session.signingKeyId()) return

  const identityId = session.identityId()!
  const signingKeyId = session.signingKeyId()!
  const secretKey = loadSigningKeySeed(identityId)
  if (!secretKey) {
    error.value =
      "This device doesn't have your signing key — recover it from your recovery phrase on Profile first, then try again."
    return
  }

  error.value = ''
  submitting.value = true
  try {
    await submitCrossNodeLoginGrant(node, identityId, signingKeyId, secretKey, node, requestingContext.value, code)
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
    await denyCrossNodeLogin(node, code)
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
  <div :class="styles.page">
    <header :class="styles.pageHeader">
      <h1 :class="styles.title">Sign in on another node</h1>
      <p :class="styles.subtitle">
        Approve a login request from a different Avalon node — a game, a console, or a browser
        that isn't already signed in as you.
      </p>
    </header>

    <AvalonCard title="Enter the request">
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
        <div v-if="integratorVerified" :class="[local.context, local.verified]">
          <AvalonIcon name="shield" :class="local.verifiedIcon" />
          <div>
            <span :class="local.contextLabel">Verified requester</span>
            <span :class="local.contextValue">{{ displayName }}</span>
          </div>
        </div>
        <AvalonWarningBanner
          v-else
          title="Unverified requester"
          :message="`${requestingContext} is not a registered integrator on this network. Only approve if you recognize it.`"
        />
        <div :class="local.context">
          <span :class="local.contextLabel">Requesting node</span>
          <span :class="local.contextValue">{{ requestingContext }}</span>
        </div>
        <div :class="local.actions">
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
        <p v-if="lookupStatus" :class="local.notPending">
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
        <div :class="local.actions">
          <AvalonButton
            label="Look up"
            variant="primary"
            :disabled="looking || !nodeBaseUrl.trim() || !userCode.trim()"
            @click="onLookUp"
          />
        </div>
      </template>
    </AvalonCard>
  </div>
</template>
