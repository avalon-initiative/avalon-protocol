<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { getMe, updateProfile } from '../api/client'
import { useSessionStore } from '../stores/session'
import { AvalonAuthCard, AvalonButton, AvalonForm, AvalonTextField } from '@avalon/ui'

const router = useRouter()
const session = useSessionStore()

const identityId = ref('')
const displayName = ref('')
const avatarUrl = ref('')
const loading = ref(true)
const submitting = ref(false)
const error = ref('')

onMounted(async () => {
  if (!session.token) return
  try {
    const profile = await getMe(session.token)
    identityId.value = profile.identity_id
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

async function onLogout() {
  session.logout()
  await router.push({ name: 'login' })
}
</script>

<template>
  <AvalonAuthCard v-if="!loading" title="Your profile" :subtitle="identityId">
    <AvalonForm submit-label="Save changes" :submitting="submitting" :error="error" @submit="onSubmit">
      <AvalonTextField v-model="displayName" label="Display name" />
      <AvalonTextField v-model="avatarUrl" label="Avatar URL" placeholder="https://…" />
    </AvalonForm>
    <RouterLink to="/friends">Friends</RouterLink>
    <AvalonButton label="Log out" variant="secondary" @click="onLogout" />
  </AvalonAuthCard>
</template>
