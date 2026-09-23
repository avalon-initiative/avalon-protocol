// The caller's own conversation list — loads once, then polls,
// same "no WebSocket needed for milestone 1" shape useMyConnections.ts
// establishes. Also resolves display names for every participant seen
// across the list (via GET /identities/profiles), so
// Messages.vue never has to know an identity by its raw id, and — same
// live-presence pattern useFriendsPresence.ts uses — subscribes to every
// participant's presence so a chat message can show their current status.
import { onMounted, onUnmounted, ref } from 'vue'
import type { Conversation, PresenceStatus, PresenceSubscription, PresenceUpdate } from '@avalon/sdk'
import { otherParticipants } from '../api/conversations'
import { useSessionStore } from '../api/session'

const POLL_INTERVAL_MS = 15_000

export function useConversations() {
  const session = useSessionStore()

  const conversations = ref<Conversation[]>([])
  const participantNames = ref<Record<string, string>>({})
  const participantPresence = ref<Record<string, PresenceStatus>>({})
  const selfId = ref('')
  const loading = ref(true)
  const error = ref('')

  let pollHandle: ReturnType<typeof setInterval> | undefined
  let presenceSubscription: PresenceSubscription | undefined

  function onPresenceUpdate(presence: PresenceUpdate) {
    participantPresence.value = { ...participantPresence.value, [presence.identityId]: presence.status }
  }

  async function resolveParticipantNames(list: Conversation[]) {
    const s = session.session
    if (!s) return
    const unknown = [
      ...new Set(list.flatMap((c) => otherParticipants(c, selfId.value))),
    ].filter((id) => !(id in participantNames.value))
    if (unknown.length === 0) return
    try {
      const profiles = await s.profiles(unknown)
      const resolved: Record<string, string> = {}
      for (const profile of Array.isArray(profiles) ? profiles : []) {
        resolved[profile.identityId] = profile.displayName
      }
      participantNames.value = { ...participantNames.value, ...resolved }
    } catch {
      // Best-effort — the list still renders with raw ids.
    }
  }

  async function refresh() {
    const s = session.session
    if (!s) return
    try {
      if (!selfId.value) {
        selfId.value = s.identity().id
        // Own messages never go through resolveParticipantNames (it only
        // ever resolves *other* participants), so seed this directly —
        // otherwise the caller's own name never appears in their own
        // conversations, only the other side's.
        participantNames.value = { ...participantNames.value, [s.identity().id]: s.profile().displayName }
      }
      const result = await s.listConversations()
      conversations.value = Array.isArray(result) ? result : []
      await resolveParticipantNames(conversations.value)
      const otherIds = [
        ...new Set(conversations.value.flatMap((c) => otherParticipants(c, selfId.value))),
      ]
      presenceSubscription?.subscribe(otherIds)
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    }
  }

  // Starts a conversation with `identityId` (or opens the existing one, per
  // POST /conversations's idempotent-on-participant-set behavior) and folds
  // it into the local list/name map immediately, rather than waiting for
  // the next poll tick.
  async function startConversation(identityId: string): Promise<Conversation | null> {
    const s = session.session
    if (!s) return null
    const conversation = await s.createConversation([identityId])
    if (!conversations.value.some((c) => c.id === conversation.id)) {
      conversations.value = [conversation, ...conversations.value]
    }
    await resolveParticipantNames([conversation])
    presenceSubscription?.subscribe(otherParticipants(conversation, selfId.value))
    return conversation
  }

  onMounted(async () => {
    const s = session.session
    if (s) {
      presenceSubscription = s.subscribePresence(onPresenceUpdate)
    }
    await refresh()
    loading.value = false
    pollHandle = setInterval(refresh, POLL_INTERVAL_MS)
  })

  onUnmounted(() => {
    if (pollHandle) clearInterval(pollHandle)
    presenceSubscription?.close()
  })

  return {
    conversations,
    participantNames,
    participantPresence,
    selfId,
    loading,
    error,
    refresh,
    startConversation,
  }
}
