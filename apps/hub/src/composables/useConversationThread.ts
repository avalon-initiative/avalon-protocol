// One conversation's message thread (issue #105) — cursor-paginated
// history (before/limit, matching crates/server/src/conversations.rs),
// newest at the bottom, load-older on demand, new messages arrive live over
// the /ws/messages socket (issue #438) rather than a poll. Same shape as
// useGuildChat.ts, minus everything that's guild-specific (no channel/role/
// permission lookups, and conversations have no moderation-delete endpoint
// — see conversations.rs's module doc comment).
//
// `conversationId` is expected to change while this composable stays
// mounted, the same way useGuildChat's `channelId` does — Messages.vue
// swaps which conversation is open without a route remount. An empty
// `conversationId` (nothing selected yet) is a valid, quiet state.
import { computed, onMounted, onUnmounted, ref, watch, type Ref } from 'vue'
import * as api from '../api/client'
import { toOldestFirst } from '../api/conversations'
import type { ConversationMessageResponse } from '../api/types'
import { useSessionStore } from '../stores/session'

const MESSAGE_PAGE_SIZE = 50

export function useConversationThread(conversationId: Ref<string>) {
  const session = useSessionStore()

  const messages = ref<ConversationMessageResponse[]>([]) // oldest-first
  const loading = ref(true)
  const loadingOlder = ref(false)
  const hasMoreOlder = ref(true)
  const error = ref('')
  const sendError = ref('')
  const sending = ref(false)

  let chatSocket: api.ChatSocket | undefined

  function closeChatSocket() {
    chatSocket?.close()
    chatSocket = undefined
  }

  // Same staleness guard useGuildChat uses: a slow request for a
  // conversation the reader already navigated away from must never
  // overwrite state for whatever they're looking at now.
  function isStaleFor(targetId: string): boolean {
    return conversationId.value !== targetId
  }

  const isEmpty = computed(() => !loading.value && messages.value.length === 0)

  async function loadLatestMessages(targetId: string) {
    if (!session.token || !targetId) return
    const page = await api.listConversationMessages(session.token, targetId, {
      limit: MESSAGE_PAGE_SIZE,
    })
    if (isStaleFor(targetId)) return
    messages.value = toOldestFirst(page)
    hasMoreOlder.value = page.length === MESSAGE_PAGE_SIZE
  }

  // Opens the live socket for `targetId` (issue #438) — closes whatever
  // was subscribed before, so switching conversations never leaves a stale
  // connection pushing updates for one the reader left.
  function subscribeToConversation(targetId: string) {
    closeChatSocket()
    if (!session.token || !targetId) return
    chatSocket = api.openConversationMessageSocket(session.token, targetId, (message) => {
      if (isStaleFor(targetId)) return
      if (messages.value.some((m) => m.id === message.id)) return
      messages.value = [...messages.value, message]
    })
  }

  async function loadOlder() {
    const targetId = conversationId.value
    if (
      !session.token ||
      !targetId ||
      loadingOlder.value ||
      !hasMoreOlder.value ||
      messages.value.length === 0
    ) {
      return
    }
    loadingOlder.value = true
    try {
      const oldestId = messages.value[0].id
      const page = await api.listConversationMessages(session.token, targetId, {
        before: oldestId,
        limit: MESSAGE_PAGE_SIZE,
      })
      if (isStaleFor(targetId)) return
      hasMoreOlder.value = page.length === MESSAGE_PAGE_SIZE
      messages.value = [...toOldestFirst(page), ...messages.value]
    } catch (e) {
      if (!isStaleFor(targetId)) {
        error.value = e instanceof Error ? e.message : 'Something went wrong.'
      }
    } finally {
      loadingOlder.value = false
    }
  }

  async function sendMessage(body: string) {
    const targetId = conversationId.value
    if (!session.token || !targetId) return
    sendError.value = ''
    sending.value = true
    try {
      const message = await api.sendConversationMessage(session.token, targetId, { body })
      // Same race as useGuildChat.ts's sendMessage: the websocket push for
      // this message can arrive before this response does — dedup against
      // it, same guard subscribeToConversation's onMessage uses.
      if (!isStaleFor(targetId) && !messages.value.some((m) => m.id === message.id)) {
        messages.value = [...messages.value, message]
      }
    } catch (e) {
      if (!isStaleFor(targetId)) {
        sendError.value = e instanceof Error ? e.message : 'Something went wrong.'
      }
    } finally {
      sending.value = false
    }
  }

  async function load() {
    const targetId = conversationId.value
    if (!session.token) return
    if (!targetId) {
      messages.value = []
      loading.value = false
      closeChatSocket()
      return
    }
    loading.value = true
    error.value = ''
    try {
      await loadLatestMessages(targetId)
      if (isStaleFor(targetId)) return
      subscribeToConversation(targetId)
    } catch (e) {
      if (!isStaleFor(targetId)) {
        error.value = e instanceof Error ? e.message : 'Something went wrong.'
      }
    } finally {
      if (!isStaleFor(targetId)) {
        loading.value = false
      }
    }
  }

  watch(conversationId, load)

  onMounted(load)

  onUnmounted(() => {
    closeChatSocket()
  })

  return {
    messages,
    loading,
    loadingOlder,
    hasMoreOlder,
    isEmpty,
    error,
    sendError,
    sending,
    loadOlder,
    sendMessage,
  }
}
