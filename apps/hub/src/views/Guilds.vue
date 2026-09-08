<script setup lang="ts">
// "My guilds" landing page (issue #24): every guild the caller belongs to
// (GET /me/guilds), plus a button-first "create guild" form — same
// read-only-until-action shape as Friends.vue's "Add a friend".
import { ref } from 'vue'
import { useRouter } from 'vue-router'
import { AvalonButton, AvalonCard, AvalonForm, AvalonGuildCard, AvalonTextField } from '@avalon/ui'
import * as api from '../api/client'
import { useMyGuilds } from '../composables/useMyGuilds'
import { useSessionStore } from '../stores/session'
import styles from './page.module.scss'

const router = useRouter()
const session = useSessionStore()
const { guilds, loading, error, refresh } = useMyGuilds()

const showCreateGuild = ref(false)
const createName = ref('')
const createTag = ref('')
const createDescription = ref('')
const creating = ref(false)
const createError = ref('')

function cancelCreateGuild() {
  showCreateGuild.value = false
  createName.value = ''
  createTag.value = ''
  createDescription.value = ''
  createError.value = ''
}

async function onCreateGuild() {
  if (!session.token) return
  createError.value = ''
  creating.value = true
  try {
    const guild = await api.createGuild(session.token, {
      name: createName.value.trim(),
      tag: createTag.value.trim(),
      description: createDescription.value.trim(),
    })
    cancelCreateGuild()
    await refresh()
    router.push({ name: 'guild', params: { id: guild.id } })
  } catch (e) {
    createError.value = e instanceof Error ? e.message : 'Something went wrong.'
  } finally {
    creating.value = false
  }
}

function openGuild(guildId: string) {
  router.push({ name: 'guild', params: { id: guildId } })
}
</script>

<template>
  <div v-if="!loading" :class="styles.page">
    <header :class="styles.pageHeader">
      <h1 :class="styles.title">Guilds</h1>
      <p :class="styles.subtitle">Communities you belong to, wherever their members are playing.</p>
    </header>
    <p v-if="error" :class="styles.error">{{ error }}</p>

    <div :class="styles.grid">
      <div :class="styles.mainColumn">
        <AvalonCard :title="`My guilds (${guilds.length})`">
          <p v-if="guilds.length === 0" :class="styles.empty">
            You're not in any guilds yet — create one to get started.
          </p>
          <AvalonGuildCard
            v-for="guild in guilds"
            :key="guild.id"
            :name="guild.name"
            :tag="guild.tag"
            :description="guild.description"
            :member-count="guild.member_count"
            @select="openGuild(guild.id)"
          />
        </AvalonCard>
      </div>

      <div :class="styles.sideColumn">
        <AvalonCard title="Create a guild">
          <AvalonButton
            v-show="!showCreateGuild"
            label="Create a guild"
            variant="primary"
            @click="showCreateGuild = true"
          />
          <div v-show="showCreateGuild">
            <AvalonForm
              submit-label="Create guild"
              :submitting="creating"
              :error="createError"
              @submit="onCreateGuild"
            >
              <AvalonTextField v-model="createName" label="Name" placeholder="Ashen Vanguard" />
              <AvalonTextField v-model="createTag" label="Tag (2-5 characters)" placeholder="ASHV" />
              <AvalonTextField
                v-model="createDescription"
                label="Description (optional)"
                placeholder="What's this guild about?"
              />
            </AvalonForm>
            <AvalonButton label="Cancel" variant="secondary" @click="cancelCreateGuild" />
          </div>
        </AvalonCard>
      </div>
    </div>
  </div>
</template>
