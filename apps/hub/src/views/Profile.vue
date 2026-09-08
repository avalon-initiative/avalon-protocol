<script setup lang="ts">
// Identity/display-name editing — the "you" tab inside the shell (issue
// #130). The identity header (display name, identity id, logout) lives in
// HubShell.vue now, shown on every tab, not just this one.
import { onMounted, ref } from 'vue'
import { getMe, updateProfile } from '../api/client'
import { useSessionStore } from '../stores/session'
import { AvalonForm, AvalonTextField } from '@avalon/ui'

const session = useSessionStore()

const displayName = ref('')
const avatarUrl = ref('')
const loading = ref(true)
const submitting = ref(false)
const error = ref('')

onMounted(async () => {
  if (!session.token) return
  try {
    const profile = await getMe(session.token)
    displayName.value = profile.display_name
    avatarUrl.value = profile.avatar_url ?? ''
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    loading.value = false
  }
})

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
  </section>
</template>
