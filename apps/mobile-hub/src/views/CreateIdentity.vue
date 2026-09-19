<script setup lang="ts">
// Same identity-creation ceremony as apps/hub's CreateIdentity.vue (#55),
// composed from the same @avalon/ui components and the shared
// @avalon/api-client module (#60).
import { ref } from 'vue'
import { useRouter } from 'vue-router'
import { AvalonAuthCard, AvalonButton, AvalonForm, AvalonTextField, AvalonWarningBanner } from '@avalon/ui'
import { createIdentity, login, useSessionStore } from '@avalon/api-client'
import AuthLayout from './AuthLayout.vue'
import styles from '../styles/CreateIdentity.module.scss'

const router = useRouter()
const session = useSessionStore()

const displayName = ref('')
const deviceLabel = ref('')
const submitting = ref(false)
const error = ref('')

// Login is identity-id-first (see docs/architecture/identity.md) — save it
// explicitly before leaving this screen, same reasoning as apps/hub.
const createdIdentityId = ref('')
const copied = ref(false)

// The signing key's BIP39 recovery phrase (#134) — shown exactly once.
const signingKeyMnemonic = ref('')
const mnemonicCopied = ref(false)

async function onSubmit() {
  error.value = ''
  submitting.value = true
  try {
    const { identityId, signingKeyMnemonic: mnemonic } = await createIdentity(
      displayName.value,
      deviceLabel.value.trim() || null,
    )
    createdIdentityId.value = identityId
    signingKeyMnemonic.value = mnemonic
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

async function copyMnemonic() {
  await navigator.clipboard.writeText(signingKeyMnemonic.value)
  mnemonicCopied.value = true
}

async function continueToHome() {
  const { token } = await login(createdIdentityId.value)
  await session.login(token, createdIdentityId.value, null)
  await router.push({ name: 'home' })
}
</script>

<template>
  <AuthLayout>
    <AvalonAuthCard
      v-if="createdIdentityId"
      title="Save your identity id"
      subtitle="You'll need this exact id to log in later."
    >
      <code :class="styles.secret">{{ createdIdentityId }}</code>
      <div :class="styles.actions">
        <AvalonButton :label="copied ? 'Copied' : 'Copy'" variant="secondary" @click="copyIdentityId" />
      </div>

      <AvalonWarningBanner
        tone="danger"
        title="You only have one passkey right now"
        message="Until you register a second one, losing this device — or losing access to this passkey any other way — means permanently losing this identity and everything durable it carries."
      />

      <h2 :class="styles.sectionTitle">Save your recovery phrase</h2>
      <p :class="styles.hint">
        This is the only time you'll see this phrase — write it down and keep it somewhere safe.
      </p>
      <code :class="styles.secret">{{ signingKeyMnemonic }}</code>
      <div :class="styles.actions">
        <AvalonButton
          :label="mnemonicCopied ? 'Copied' : 'Copy'"
          variant="secondary"
          @click="copyMnemonic"
        />
        <AvalonButton label="Continue" variant="primary" @click="continueToHome" />
      </div>
    </AvalonAuthCard>
    <AvalonAuthCard
      v-else
      title="Create your Avalon identity"
      subtitle="One identity, every integrator connected to Avalon."
    >
      <AvalonForm submit-label="Create identity" :submitting="submitting" :error="error" @submit="onSubmit">
        <AvalonTextField
          v-model="displayName"
          label="Display name"
          placeholder="How other users see you"
        />
        <AvalonTextField
          v-model="deviceLabel"
          label="This device's name (optional)"
          placeholder="e.g. Phone"
        />
      </AvalonForm>
      <p :class="styles.switchLink">
        <RouterLink to="/login">Already have an identity? Log in</RouterLink>
      </p>
    </AvalonAuthCard>
  </AuthLayout>
</template>
