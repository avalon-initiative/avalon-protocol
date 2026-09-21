<script setup lang="ts">
// Direct-message UI (issue #105): a persistent conversation-list sidebar
// next to the active conversation's thread — the same "swap selection in
// place, no remount" shape #241 established for Guild.vue's Channels tab.
// Reuses the exact same message components guild chat does
// (AvalonChatMessage/AvalonChatComposer are wire-shape-agnostic — see
// their own prop types) so this view is glue only: no new packages/ui
// component, per this ticket's invariant.
import { computed, nextTick, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { AvalonCard, AvalonChatComposer, AvalonChatMessage } from '@avalon/ui'
import { otherParticipants, MESSAGE_BODY_MAX_CHARS } from '../api/conversations'
import { useConversations } from '../composables/useConversations'
import { useConversationThread } from '../composables/useConversationThread'
import styles from '../styles/page.module.scss'
import local from '../styles/Messages.module.scss'

const route = useRoute()
const router = useRouter()

const {
  conversations,
  participantNames,
  participantPresence,
  selfId,
  loading: listLoading,
  error: listError,
} = useConversations()

const selectedId = ref((route.params.id as string) ?? '')
watch(
  () => route.params.id,
  (id) => {
    selectedId.value = (id as string) ?? ''
  },
)

function selectConversation(id: string) {
  selectedId.value = id
  router.replace({ name: 'conversation', params: { id } })
}

// Label for a conversation row / the open thread's header — the other
// participant's name for a 1:1, joined names for a group. Falls back to
// the raw id for anyone not yet resolved (issue #161's same posture as
// guild chat's authorNames).
function conversationLabel(id: string): string {
  const conversation = conversations.value.find((c) => c.id === id)
  if (!conversation) return id
  const others = otherParticipants(conversation, selfId.value)
  return others.map((pid) => participantNames.value[pid] ?? pid).join(', ')
}

const activeLabel = computed(() => (selectedId.value ? conversationLabel(selectedId.value) : ''))

// Same persistent-scroll-container reasoning as Guild.vue's Channels tab —
// reused across conversation switches, not remounted.
const messageScrollEl = ref<HTMLElement | null>(null)

const NEAR_BOTTOM_THRESHOLD_PX = 120

function isNearBottom(el: HTMLElement): boolean {
  return el.scrollHeight - el.scrollTop - el.clientHeight < NEAR_BOTTOM_THRESHOLD_PX
}

function scrollToBottom() {
  if (messageScrollEl.value) {
    messageScrollEl.value.scrollTop = messageScrollEl.value.scrollHeight
  }
}

const {
  messages,
  loading: threadLoading,
  loadingOlder,
  hasMoreOlder,
  isEmpty,
  error: threadError,
  sendError,
  sending,
  loadOlder,
  sendMessage,
} = useConversationThread(selectedId)

// Same "force bottom on switch, follow only if already at the bottom"
// shape Guild.vue's Channels tab uses — see that view's own comment for
// why this is a plain flag rather than watching `threadLoading` directly.
let forceScrollOnNextMessages = false
watch(selectedId, () => {
  forceScrollOnNextMessages = true
})

watch(
  () => messages.value.length,
  (newLen, oldLen) => {
    const el = messageScrollEl.value
    const wasNearBottom = !el || isNearBottom(el)
    if (forceScrollOnNextMessages) {
      if (newLen === 0) return
      forceScrollOnNextMessages = false
      nextTick(scrollToBottom)
      return
    }
    if (newLen > oldLen && wasNearBottom) {
      nextTick(scrollToBottom)
    }
  },
)

const draft = ref('')

async function onSendMessage() {
  const body = draft.value
  draft.value = ''
  await sendMessage(body)
}

function onMessageScroll(event: Event) {
  const el = event.target as HTMLElement
  if (el.scrollTop < 80 && hasMoreOlder.value && !loadingOlder.value) {
    loadOlder()
  }
}
</script>

<template>
  <div v-if="!listLoading" :class="styles.page">
    <header :class="styles.pageHeader">
      <h1 :class="styles.title">Messages</h1>
      <p :class="styles.subtitle">Direct conversations with your friends and guildmates.</p>
    </header>
    <p v-if="listError" :class="styles.error">{{ listError }}</p>

    <div :class="local.layout">
      <div :class="local.sidebar">
        <p v-if="conversations.length === 0" :class="styles.empty">
          No conversations yet — message a friend from the Friends page to start one.
        </p>
        <button
          v-for="conversation in conversations"
          :key="conversation.id"
          type="button"
          :class="[local.conversationRow, conversation.id === selectedId && local.conversationActive]"
          @click="selectConversation(conversation.id)"
        >
          {{ conversationLabel(conversation.id) }}
        </button>
      </div>

      <div :class="local.thread">
        <p v-if="!selectedId" :class="styles.empty">
          Select a conversation, or message a friend from the Friends page.
        </p>
        <template v-else>
          <p v-if="threadError" :class="styles.error">{{ threadError }}</p>
          <p v-if="threadLoading" :class="styles.empty">Loading conversation…</p>
          <AvalonCard v-else :title="activeLabel">
            <div ref="messageScrollEl" :class="local.messageScroll" @scroll="onMessageScroll">
              <p v-if="loadingOlder" :class="styles.empty">Loading older messages…</p>
              <p v-else-if="!hasMoreOlder && messages.length > 0" :class="styles.empty">
                Start of this conversation.
              </p>
              <p v-if="isEmpty" :class="styles.empty">No messages yet — say hello.</p>
              <AvalonChatMessage
                v-for="message in messages"
                :key="message.id"
                :author-id="message.author"
                :author-display-name="participantNames[message.author]"
                :body="message.body"
                :sent-at-label="new Date(message.sentAt).toLocaleString()"
                :is-own="message.author === selfId"
                :presence-status="participantPresence[message.author]"
              />
            </div>

            <AvalonChatComposer
              v-model="draft"
              :max-chars="MESSAGE_BODY_MAX_CHARS"
              :sending="sending"
              :error="sendError"
              @send="onSendMessage"
            />
          </AvalonCard>
        </template>
      </div>
    </div>
  </div>
</template>
