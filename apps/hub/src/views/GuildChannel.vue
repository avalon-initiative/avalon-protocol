<script setup lang="ts">
// Guild channel chat (issue #24): newest-at-bottom, load-older-on-scroll
// via #22's cursor pagination (`before`/`limit`), composer enforcing the
// server's message body cap. All loading/pagination/poll state lives in
// useGuildChat — this view owns only the scroll-to-load-older wiring and
// the composer's draft text.
import { computed, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { AvalonCard, AvalonChatComposer, AvalonChatMessage } from '@avalon/ui'
import { MESSAGE_BODY_MAX_CHARS } from '../api/guildChat'
import { useGuildChat } from '../composables/useGuildChat'
import local from './GuildChannel.module.scss'
import styles from './page.module.scss'

const route = useRoute()
const router = useRouter()

const guildId = computed(() => route.params.id as string)
const channelId = computed(() => route.params.cid as string)

const {
  guild,
  channel,
  messages,
  canDelete,
  loading,
  loadingOlder,
  hasMoreOlder,
  error,
  sendError,
  sending,
  loadOlder,
  sendMessage,
  deleteMessage,
} = useGuildChat(guildId, channelId)

const draft = ref('')

async function onSend() {
  const body = draft.value
  draft.value = ''
  await sendMessage(body)
}

// Loads the next older page once the reader scrolls near the top of the
// history, rather than a separate "load more" button — matches the
// ticket's "load-older on scroll" design.
function onScroll(event: Event) {
  const el = event.target as HTMLElement
  if (el.scrollTop < 80 && hasMoreOlder.value && !loadingOlder.value) {
    loadOlder()
  }
}

function backToGuild() {
  router.push({ name: 'guild', params: { id: guildId.value } })
}
</script>

<template>
  <div v-if="!loading" :class="styles.page">
    <header :class="styles.pageHeader">
      <button :class="local.backLink" type="button" @click="backToGuild">&larr; Back to {{ guild?.name }}</button>
      <h1 :class="styles.title">#{{ channel?.name ?? 'channel' }}</h1>
      <p v-if="channel?.archived" :class="styles.subtitle">
        This channel is archived — history is readable, but new messages can't be sent.
      </p>
      <p :class="styles.subtitle">
        Message history is kept up to the server's retention policy, not permanent.
      </p>
    </header>

    <p v-if="error" :class="styles.error">{{ error }}</p>

    <AvalonCard>
      <div :class="local.messageScroll" @scroll="onScroll">
        <p v-if="loadingOlder" :class="styles.empty">Loading older messages…</p>
        <p v-else-if="!hasMoreOlder && messages.length > 0" :class="styles.empty">Start of channel history.</p>
        <p v-if="messages.length === 0" :class="styles.empty">No messages yet — say hello.</p>
        <AvalonChatMessage
          v-for="message in messages"
          :key="message.id"
          :author-id="message.author"
          :body="message.body"
          :sent-at-label="new Date(message.sent_at).toLocaleString()"
          :can-delete="canDelete"
          @delete="deleteMessage(message.id)"
        />
      </div>

      <AvalonChatComposer
        v-model="draft"
        :max-chars="MESSAGE_BODY_MAX_CHARS"
        :sending="sending"
        :error="sendError"
        :disabled="channel?.archived ?? false"
        @send="onSend"
      />
    </AvalonCard>
  </div>
</template>
