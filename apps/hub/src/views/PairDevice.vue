<script setup lang="ts">
// "Pair a device" (issue #307): lets the user approve a WebAuthn-incapable
// client's pairing request (a game engine, a console) from their already
// authenticated Hub session — no new auth surface for the Hub itself. The
// `user_code` is pre-filled when this view is reached via a
// `verification_uri` with `?user_code=...` (e.g. from a QR code), but can
// also be typed in by hand.
import { ref } from 'vue'
import { useRoute } from 'vue-router'
import { AvalonButton, AvalonCard, AvalonTextField } from '@avalon/ui'
import * as api from '@avalon/api-client'
import { useSessionStore } from '@avalon/api-client'
import local from '../styles/PairDevice.module.scss'
import styles from '../styles/page.module.scss'

const route = useRoute()
const session = useSessionStore()

const userCode = ref(typeof route.query.user_code === 'string' ? route.query.user_code : '')
const submitting = ref(false)
const error = ref('')
const resolvedStatus = ref<'approved' | 'denied' | null>(null)

async function onApprove() {
  if (!session.token || !userCode.value.trim()) return
  error.value = ''
  submitting.value = true
  try {
    const result = await api.approvePairing(session.token, { user_code: userCode.value.trim() })
    resolvedStatus.value = result.status
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    submitting.value = false
  }
}

async function onDeny() {
  if (!session.token || !userCode.value.trim()) return
  error.value = ''
  submitting.value = true
  try {
    const result = await api.denyPairing(session.token, { user_code: userCode.value.trim() })
    resolvedStatus.value = result.status
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    submitting.value = false
  }
}

function onStartOver() {
  resolvedStatus.value = null
  userCode.value = ''
  error.value = ''
}
</script>

<template>
  <div :class="styles.page">
    <header :class="styles.pageHeader">
      <h1 :class="styles.title">Pair a device</h1>
      <p :class="styles.subtitle">
        Approve a code shown on another device (an integrator, a console) to sign it in as you.
      </p>
    </header>

    <AvalonCard title="Enter the code">
      <template v-if="resolvedStatus === 'approved'">
        <p>Device paired. It should sign in automatically within a few seconds.</p>
        <AvalonButton label="Pair another device" variant="secondary" @click="onStartOver" />
      </template>
      <template v-else-if="resolvedStatus === 'denied'">
        <p>Pairing denied.</p>
        <AvalonButton label="Pair another device" variant="secondary" @click="onStartOver" />
      </template>
      <template v-else>
        <p v-if="error" :class="styles.error">{{ error }}</p>
        <AvalonTextField
          v-model="userCode"
          label="Code"
          placeholder="ABCD2345"
          :disabled="submitting"
        />
        <div :class="local.actions">
          <AvalonButton
            label="Approve"
            variant="primary"
            :disabled="submitting || !userCode.trim()"
            @click="onApprove"
          />
          <AvalonButton
            label="Deny"
            variant="danger"
            :disabled="submitting || !userCode.trim()"
            @click="onDeny"
          />
        </div>
      </template>
    </AvalonCard>
  </div>
</template>
