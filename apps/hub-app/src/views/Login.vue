<script setup lang="ts">
// Identity-id-first login ceremony, driven by the SDK's `loginWithIdentityId`.
import { ref } from 'vue'
import { useRouter } from 'vue-router'
import { AvalonAuthCard, AvalonForm, AvalonTextField } from '@avalon-initiative/common-ui'
import { avalonClient, useSessionStore } from '../api/session'
import { loadSigningKeySeed } from '../api/signingKeyStorage'
import AuthLayout from './AuthLayout.vue'
import styles from '../styles/CreateIdentity.module.scss'

const router = useRouter()
const session = useSessionStore()

const identityId = ref('')
const submitting = ref(false)
const error = ref('')

async function onSubmit() {
  error.value = ''
  submitting.value = true
  try {
    const accountSession = await avalonClient().loginWithIdentityId(identityId.value)
    // The reconnect-across-nodes signing key, when this device already holds
    // one for this identity; a device without one logs in without it.
    const secretKey = loadSigningKeySeed(identityId.value)
    if (secretKey) await accountSession.attachSigningKey(secretKey)
    await session.setSession(accountSession)
    await router.push({ name: 'home' })
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    submitting.value = false
  }
}
</script>

<template>
  <AuthLayout>
    <AvalonAuthCard title="Welcome to Avalon" subtitle="Your identity, wherever you are.">
      <AvalonForm submit-label="Log in with passkey" :submitting="submitting" :error="error" @submit="onSubmit">
        <AvalonTextField v-model="identityId" label="Identity id" placeholder="Your identity id" />
      </AvalonForm>
      <p :class="styles.switchLink">
        <RouterLink to="/create-identity">Don't have an identity yet? Create one</RouterLink>
      </p>
      <p :class="styles.switchLink">
        <RouterLink to="/settings">Server settings</RouterLink>
      </p>
    </AvalonAuthCard>
  </AuthLayout>
</template>
