<script setup lang="ts">
// Same identity-id-first login ceremony as apps/hub's Login.vue,
// composed from the same @avalon/ui components and the shared
// @avalon/api-client module — no local WebAuthn/session logic here.
import { ref } from 'vue'
import { useRouter } from 'vue-router'
import { AvalonAuthCard, AvalonForm, AvalonTextField } from '@avalon/ui'
import { login, useSessionStore } from '@avalon/api-client'
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
    // This device's local signing key (if any) is resolved lazily later,
    // same as apps/hub — reconnect-across-nodes support just isn't
    // available until then, which is fine, not required for login itself.
    await session.login(token, identityId.value, null)
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
