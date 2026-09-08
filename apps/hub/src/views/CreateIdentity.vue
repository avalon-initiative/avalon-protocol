<script setup lang="ts">
import { ref } from 'vue'
import { useRouter } from 'vue-router'
import { AvalonAuthCard, AvalonForm, AvalonTextField } from '@avalon/ui'
import { createIdentity, login } from '../api/identity'
import { useSessionStore } from '../stores/session'

const router = useRouter()
const session = useSessionStore()

const displayName = ref('')
const submitting = ref(false)
const error = ref('')

async function onSubmit() {
  error.value = ''
  submitting.value = true
  try {
    const { identityId } = await createIdentity(displayName.value)
    // Registration only proves the passkey/signing-key ceremony; it does not
    // itself start a session — log in immediately after so the new player
    // lands authenticated rather than being sent to a second manual step.
    const { token } = await login(identityId)
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
  <AvalonAuthCard
    title="Create your Avalon identity"
    subtitle="One identity, every game connected to Avalon."
  >
    <AvalonForm submit-label="Create identity" :submitting="submitting" :error="error" @submit="onSubmit">
      <AvalonTextField
        v-model="displayName"
        label="Display name"
        placeholder="How other players see you"
      />
    </AvalonForm>
    <RouterLink to="/login">Already have an identity? Log in</RouterLink>
  </AvalonAuthCard>
</template>
