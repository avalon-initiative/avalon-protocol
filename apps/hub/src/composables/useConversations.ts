// The caller's own conversation list (issue #105) — loads once, then polls,
// same "no WebSocket needed for milestone 1" shape useMyConnections.ts and
// useGuildChat.ts already establish. Also resolves display names for every
// participant seen across the list (via GET /identities/profiles, issue
// #161), so Messages.vue never has to know an identity by its raw id.
import { onMounted, onUnmounted, ref } from 'vue'
import * as api from '../api/client'
import { otherParticipants } from '../api/conversations'
import type { ConversationResponse } from '../api/types'
import { useSessionStore } from '../stores/session'

const POLL_INTERVAL_MS = 15_000

export function useConversations() {
  const session = useSessionStore()

  const conversations = ref<ConversationResponse[]>([])
  const participantNames = ref<Record<string, string>>({})
  const selfId = ref('')
  const loading = ref(true)
  const error = ref('')

  let pollHandle: ReturnType<typeof setInterval> | undefined

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
        selfId.value = (await api.getMe(session.token)).identity_id
      }
      conversations.value = await api.listConversations(session.token)
      await resolveParticipantNames(conversations.value)
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
    return conversation
  }

  onMounted(async () => {
    await refresh()
    loading.value = false
    pollHandle = setInterval(refresh, POLL_INTERVAL_MS)
  })

  onUnmounted(() => {
    if (pollHandle) clearInterval(pollHandle)
  })

  return { conversations, participantNames, selfId, loading, error, refresh, startConversation }
}
