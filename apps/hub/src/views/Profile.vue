<script setup lang="ts">
// Identity/display-name editing — the "you" tab inside the shell (issue
// #130). The identity header (display name, identity id, logout) lives in
// HubShell.vue now, shown on every tab, not just this one.
import { onMounted, ref } from 'vue'
import { getMe, updateProfile } from '../api/client'
import { recoverSigningKey } from '../api/identity'
import { loadSigningKey } from '../crypto/signingKey'
import { useSessionStore } from '../stores/session'
import { AvalonForm, AvalonTextField } from '@avalon/ui'

const session = useSessionStore()

const displayName = ref('')
const avatarUrl = ref('')
const identityId = ref('')
const loading = ref(true)
const submitting = ref(false)
const error = ref('')

// #134: this device has no signing key for the current identity — either
// it's brand new, or storage was cleared. Recovering from a saved phrase
// (below) is the fallback path; the primary path is #135's
// device-registration grant model, not built yet.
const hasSigningKey = ref(true)

onMounted(async () => {
  if (!session.token) return
  try {
    const profile = await getMe(session.token)
    displayName.value = profile.display_name
    avatarUrl.value = profile.avatar_url ?? ''
    identityId.value = profile.identity_id
    hasSigningKey.value = loadSigningKey(profile.identity_id) !== null
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    loading.value = false
  }
})

const recoveryPhrase = ref('')
const recovering = ref(false)
const recoveryError = ref('')

function onRecoverSigningKey() {
  recoveryError.value = ''
  recovering.value = true
  try {
    recoverSigningKey(identityId.value, recoveryPhrase.value.trim())
    hasSigningKey.value = true
    recoveryPhrase.value = ''
  } catch (e) {
    recoveryError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    recovering.value = false
  }
}

async function onSubmit() {
  if (!session.token) return
  error.value = ''
  submitting.value = true
  try {
    const profile = await updateProfile(session.token, {
      display_name: displayName.value,
      avatar_url: avatarUrl.value,
    })
    displayName.value = profile.display_name
    avatarUrl.value = profile.avatar_url ?? ''
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    submitting.value = false
  }
}
</script>

<template>
  <section v-if="!loading">
    <AvalonForm submit-label="Save changes" :submitting="submitting" :error="error" @submit="onSubmit">
      <AvalonTextField v-model="displayName" label="Display name" />
      <AvalonTextField v-model="avatarUrl" label="Avatar URL" placeholder="https://…" />
    </AvalonForm>

    <section v-if="!hasSigningKey">
      <h2>Recover your signing key</h2>
      <p>
        This device doesn't have a signing key for this identity yet. Enter the recovery phrase
        you saved when you created your identity to restore it here.
      </p>
      <AvalonForm
        submit-label="Recover"
        :submitting="recovering"
        :error="recoveryError"
        @submit="onRecoverSigningKey"
      >
        <AvalonTextField
          v-model="recoveryPhrase"
          label="Recovery phrase"
          placeholder="twelve words separated by spaces"
        />
      </AvalonForm>
    </section>
  </section>
</template>
