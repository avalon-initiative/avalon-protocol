<script setup lang="ts">
import { ref } from 'vue'
import { useRouter } from 'vue-router'
import { AvalonAuthCard, AvalonButton, AvalonForm, AvalonTextField } from '@avalon/ui'
import { createIdentity, login } from '../api/identity'
import { useSessionStore } from '../stores/session'
import styles from './CreateIdentity.module.scss'

const router = useRouter()
const session = useSessionStore()

const displayName = ref('')
const submitting = ref(false)
const error = ref('')

// Login is identity-id-first (see docs/architecture/identity.md) — the
// display name is never enough to log back in with. A passkey manager
// autofilling the WebAuthn credential's stored username now shows the
// identity id correctly (server-side fix), but not everyone has one
// active, so this screen is a second, explicit chance to save it before
// the player ever leaves the page.
const createdIdentityId = ref('')
const copied = ref(false)

async function onSubmit() {
  error.value = ''
  submitting.value = true
  try {
    const { identityId } = await createIdentity(displayName.value)
    createdIdentityId.value = identityId
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    submitting.value = false
  }
}

async function copyIdentityId() {
  await navigator.clipboard.writeText(createdIdentityId.value)
  copied.value = true
}

async function continueToProfile() {
  // Registration only proves the passkey/signing-key ceremony; it does not
  // itself start a session — log in now that the player has acknowledged
  // their id, rather than sending them to a second manual login step.
  const { token } = await login(createdIdentityId.value)
  session.login(token)
  await router.push({ name: 'profile' })
}
</script>

<template>
  <AvalonAuthCard
    v-if="createdIdentityId"
    title="Save your identity id"
    subtitle="You'll need this exact id to log in later — your display name alone won't work."
  >
    <code :class="styles.identityId">{{ createdIdentityId }}</code>
    <div :class="styles.actions">
      <AvalonButton :label="copied ? 'Copied' : 'Copy'" variant="secondary" @click="copyIdentityId" />
      <AvalonButton label="Continue to profile" variant="primary" @click="continueToProfile" />
    </div>
    <p :class="styles.copyHint">
      If your browser or password manager saved a passkey just now, it should also remember this id
      as the login username — but save it somewhere yourself too, just in case.
    </p>
  </AvalonAuthCard>
  <AvalonAuthCard
    v-else
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
