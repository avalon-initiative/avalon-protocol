// One conversation's message thread (issue #105) — cursor-paginated
// history (before/limit, matching crates/server/src/conversations.rs),
// newest at the bottom, load-older on demand, short-poll for new messages.
// Same shape as useGuildChat.ts, minus everything that's guild-specific
// (no channel/role/permission lookups, and conversations have no
// moderation-delete endpoint — see conversations.rs's module doc comment).
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
const POLL_INTERVAL_MS = 15_000

export function useConversationThread(conversationId: Ref<string>) {
  const session = useSessionStore()

  const messages = ref<ConversationMessageResponse[]>([]) // oldest-first
  const loading = ref(true)
  const loadingOlder = ref(false)
  const hasMoreOlder = ref(true)
  const error = ref('')
  const sendError = ref('')
  const sending = ref(false)

  let pollHandle: ReturnType<typeof setInterval> | undefined

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

  async function pollNewMessages() {
    const targetId = conversationId.value
    if (!session.token || !targetId) return
    try {
      const page = await api.listConversationMessages(session.token, targetId, {
        limit: MESSAGE_PAGE_SIZE,
      })
      if (isStaleFor(targetId)) return
      const known = new Set(messages.value.map((m) => m.id))
      const fresh = toOldestFirst(page).filter((m) => !known.has(m.id))
      if (fresh.length > 0) {
        messages.value = [...messages.value, ...fresh]
      }
    } catch {
      // Best-effort poll — the next tick retries.
    }
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
      if (!isStaleFor(targetId)) {
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
      return
    }
    loading.value = true
    error.value = ''
    try {
      await loadLatestMessages(targetId)
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

  onMounted(() => {
    load()
    pollHandle = setInterval(pollNewMessages, POLL_INTERVAL_MS)
  })

  onUnmounted(() => {
    if (pollHandle) clearInterval(pollHandle)
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
