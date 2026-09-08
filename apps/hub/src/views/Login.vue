<script setup lang="ts">
import { ref } from 'vue'
import { useRouter } from 'vue-router'
import { AvalonAuthCard, AvalonForm, AvalonTextField } from '@avalon/ui'
import { login } from '../api/identity'
import { useSessionStore } from '../stores/session'

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
    session.login(token)
    await router.push({ name: 'profile' })
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    submitting.value = false
  }
}
</script>

<template>
  <AvalonAuthCard title="Log in to Avalon" subtitle="Use the passkey you registered with.">
    <AvalonForm submit-label="Log in" :submitting="submitting" :error="error" @submit="onSubmit">
      <AvalonTextField v-model="identityId" label="Identity id" placeholder="Your identity id" />
    </AvalonForm>
    <RouterLink to="/create-identity">Don't have an identity yet? Create one</RouterLink>
  </AvalonAuthCard>
</template>
