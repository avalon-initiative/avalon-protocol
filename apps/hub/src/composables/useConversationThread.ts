// One conversation's message thread — cursor-paginated
// history (before/limit, matching crates/server/src/conversations.rs),
// newest at the bottom, load-older on demand, new messages arrive live over
// the /ws/messages socket rather than a poll. Same shape as
// useGuildChat.ts, minus everything that's guild-specific (no channel/role/
// permission lookups, and conversations have no moderation-delete endpoint
// — see conversations.rs's module doc comment).
//
// `conversationId` is expected to change while this composable stays
// mounted, the same way useGuildChat's `channelId` does — Messages.vue
// swaps which conversation is open without a route remount. An empty
// `conversationId` (nothing selected yet) is a valid, quiet state.
import { computed, onMounted, onUnmounted, ref, watch, type Ref } from 'vue'
import type { ConversationMessage, RealtimeSubscription } from '@avalon-initiative/protocol-sdk'
import { toOldestFirst } from '../api/conversations'
import { markConversationSeen } from '../api/notifications'
import { useSessionStore } from '../api/session'

const MESSAGE_PAGE_SIZE = 50

export function useConversationThread(conversationId: Ref<string>) {
  const session = useSessionStore()

  const messages = ref<ConversationMessage[]>([]) // oldest-first
  const loading = ref(true)
  const loadingOlder = ref(false)
  const hasMoreOlder = ref(true)
  const error = ref('')
  const sendError = ref('')
  const sending = ref(false)

  let chatSocket: RealtimeSubscription | undefined

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
    const s = session.session
    if (!s || !targetId) return
    const page = await s.conversationMessages(targetId, undefined, MESSAGE_PAGE_SIZE)
    if (isStaleFor(targetId)) return
    const rows = Array.isArray(page) ? page : []
    messages.value = toOldestFirst(rows)
    hasMoreOlder.value = rows.length === MESSAGE_PAGE_SIZE
    // Opening a conversation is what "reads" it for unread-DM
    // tracking — mirrors HubShell.vue's onSelectAnnouncement marking a
    // guild announcement's channel seen the moment it's actually opened.
    const newest = messages.value[messages.value.length - 1]
    if (newest) markConversationSeen(targetId, newest.sentAt)
  }

  // Opens the live socket for `targetId` — closes whatever
  // was subscribed before, so switching conversations never leaves a stale
  // connection pushing updates for one the reader left.
  function subscribeToConversation(targetId: string) {
    closeChatSocket()
    const s = session.session
    if (!s || !targetId) return
    chatSocket = s.subscribeConversationMessages(targetId, (message) => {
      if (isStaleFor(targetId)) return
      if (messages.value.some((m) => m.id === message.id)) return
      messages.value = [...messages.value, message]
      // The reader has this thread open right now — a message arriving
      // live counts as seen immediately, same as loadLatestMessages above.
      markConversationSeen(targetId, message.sentAt)
    })
  }

  async function loadOlder() {
    const targetId = conversationId.value
    const s = session.session
    if (
      !s ||
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
      const page = await s.conversationMessages(targetId, oldestId, MESSAGE_PAGE_SIZE)
      if (isStaleFor(targetId)) return
      const rows = Array.isArray(page) ? page : []
      hasMoreOlder.value = rows.length === MESSAGE_PAGE_SIZE
      messages.value = [...toOldestFirst(rows), ...messages.value]
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
    const s = session.session
    if (!s || !targetId) return
    sendError.value = ''
    sending.value = true
    try {
      const message = await s.sendConversationMessage(targetId, body)
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
    if (!session.session) return
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
