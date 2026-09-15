// The caller's own conversation list (issue #105) — loads once, then polls,
// same "no WebSocket needed for milestone 1" shape useMyConnections.ts
// establishes. Also resolves display names for every participant seen
// across the list (via GET /identities/profiles, issue #161), so
// Messages.vue never has to know an identity by its raw id, and — same
// live-presence pattern useFriendsPresence.ts uses — subscribes to every
// participant's presence so a chat message can show their current status.
import { onMounted, onUnmounted, ref } from 'vue'
import * as api from '../api/client'
import type { PresenceSocket } from '../api/client'
import { otherParticipants } from '../api/conversations'
import type { ConversationResponse, PresenceResponse, PresenceStatus } from '../api/types'
import { useSessionStore } from '../stores/session'

const POLL_INTERVAL_MS = 15_000

export function useConversations() {
  const session = useSessionStore()

  const conversations = ref<ConversationResponse[]>([])
  const participantNames = ref<Record<string, string>>({})
  const participantPresence = ref<Record<string, PresenceStatus>>({})
  const selfId = ref('')
  const loading = ref(true)
  const error = ref('')

  let pollHandle: ReturnType<typeof setInterval> | undefined
  let presenceSocket: PresenceSocket | undefined

  function onPresenceUpdate(presence: PresenceResponse) {
    participantPresence.value = { ...participantPresence.value, [presence.identity_id]: presence.status }
  }

  async function resolveParticipantNames(list: ConversationResponse[]) {
    if (!session.token) return
    const unknown = [
      ...new Set(list.flatMap((c) => otherParticipants(c, selfId.value))),
    ].filter((id) => !(id in participantNames.value))
    if (unknown.length === 0) return
    try {
      const profiles = await api.getProfiles(session.token, unknown)
      const resolved: Record<string, string> = {}
      for (const profile of profiles) {
        resolved[profile.identity_id] = `${profile.display_name}#${profile.discriminator}`
      }
      participantNames.value = { ...participantNames.value, ...resolved }
    } catch {
      // Best-effort — the list still renders with raw ids.
    }
  }

  async function refresh() {
    if (!session.token) return
    try {
      if (!selfId.value) {
        const me = await api.getMe(session.token)
        selfId.value = me.identity_id
        // Own messages never go through resolveParticipantNames (it only
        // ever resolves *other* participants), so seed this directly —
        // otherwise the caller's own name never appears in their own
        // conversations, only the other side's.
        participantNames.value = { ...participantNames.value, [me.identity_id]: me.handle }
      }
      conversations.value = await api.listConversations(session.token)
      await resolveParticipantNames(conversations.value)
      const otherIds = [
        ...new Set(conversations.value.flatMap((c) => otherParticipants(c, selfId.value))),
      ]
      presenceSocket?.subscribe(otherIds)
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    }
  }

  // Starts a conversation with `identityId` (or opens the existing one, per
  // POST /conversations's idempotent-on-participant-set behavior — see
  // client.ts::createConversation) and folds it into the local list/name
  // map immediately, rather than waiting for the next poll tick.
  async function startConversation(identityId: string): Promise<ConversationResponse | null> {
    if (!session.token) return null
    const conversation = await api.createConversation(session.token, {
      participants: [identityId],
    })
    if (!conversations.value.some((c) => c.id === conversation.id)) {
      conversations.value = [conversation, ...conversations.value]
    }
    await resolveParticipantNames([conversation])
    presenceSocket?.subscribe(otherParticipants(conversation, selfId.value))
    return conversation
  }

  onMounted(async () => {
    if (session.token) {
      presenceSocket = api.openPresenceSocket(session.token, onPresenceUpdate)
    }
    await refresh()
    loading.value = false
    pollHandle = setInterval(refresh, POLL_INTERVAL_MS)
  })

  onUnmounted(() => {
    if (pollHandle) clearInterval(pollHandle)
    presenceSocket?.close()
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
