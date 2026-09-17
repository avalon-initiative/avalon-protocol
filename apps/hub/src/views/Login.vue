<script setup lang="ts">
import { ref } from 'vue'
import { useRouter } from 'vue-router'
import { AvalonAuthCard, AvalonForm, AvalonTextField } from '@avalon/ui'
import { login } from '../api/identity'
import { findMySigningKeyId } from '../api/deviceGrants'
import { loadSigningKey } from '../crypto/signingKey'
import { useSessionStore } from '../stores/session'
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
    const { token } = await login(identityId.value)
    // Issue #525: the reconnect-across-nodes signing_key_id, when this
    // device already holds a local signing key for this identity — `null`
    // otherwise (e.g. this is a brand-new device that hasn't been granted
    // one yet), same as `session.login`'s own doc comment describes.
    const secretKey = loadSigningKey(identityId.value)
    const signingKeyId = secretKey ? await findMySigningKeyId(token, secretKey) : null
    session.login(token, identityId.value, signingKeyId)
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
    <AvalonAuthCard title="Welcome to Avalon" subtitle="Your identity. Your integrators. Your community.">
      <AvalonForm submit-label="Log in with passkey" :submitting="submitting" :error="error" @submit="onSubmit">
        <AvalonTextField v-model="identityId" label="Identity id" placeholder="Your identity id" />
      </AvalonForm>
      <p :class="styles.switchLink">
        <RouterLink to="/create-identity">Don't have an identity yet? Create one</RouterLink>
      </p>
      <p :class="styles.switchLink">
        <RouterLink to="/recover-identity">Lost every device? Recover your identity</RouterLink>
      </p>
    </AvalonAuthCard>
  </AuthLayout>
</template>
