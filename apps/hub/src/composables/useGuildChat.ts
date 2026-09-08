// Channel chat for GuildChannel.vue (issue #22/#24): cursor-paginated
// history (`before`/`limit`, matching crates/server/src/guild_messages.rs),
// newest at the bottom, load-older on demand. No WebSocket for milestone 1
// (the ticket's own design note) — new messages are picked up by
// re-fetching the latest page on a short poll and merging in anything not
// already known, since the server only exposes an older-page cursor
// (`before`), not a "since" one.
import { computed, onMounted, onUnmounted, ref, watch, type Ref } from 'vue'
import * as api from '../api/client'
import { hasGuildPermission, permissionsForMember } from '../api/guilds'
import type { GuildMember } from '../api/guilds'
import { toOldestFirst } from '../api/guildChat'
import type { ChannelResponse, GuildResponse, MessageResponse } from '../api/types'
import { useSessionStore } from '../stores/session'

const MESSAGE_PAGE_SIZE = 50
// Chat should feel more live than the 5-minute friend/guild-membership
// polls elsewhere in the Hub, but still comfortably above the server's
// own rate of change for a milestone-1, no-WebSocket poll.
const POLL_INTERVAL_MS = 15_000

export function useGuildChat(guildId: Ref<string>, channelId: Ref<string>) {
  const session = useSessionStore()

  const guild = ref<GuildResponse | null>(null)
  const channel = ref<ChannelResponse | null>(null)
  const messages = ref<MessageResponse[]>([]) // oldest-first, for newest-at-bottom rendering
  const selfId = ref('')
  const selfPermissions = ref<string[]>([])
  const loading = ref(true)
  const loadingOlder = ref(false)
  const hasMoreOlder = ref(true)
  const error = ref('')
  const sendError = ref('')
  const sending = ref(false)

  let pollHandle: ReturnType<typeof setInterval> | undefined

  const canDelete = computed(
    () =>
      guild.value !== null &&
      hasGuildPermission(guild.value, selfId.value, selfPermissions.value, 'manage_channels'),
  )

  async function loadChannelMeta() {
    if (!session.token) return
    const token = session.token
    const [guildResp, channels, roles, membersResp, profile] = await Promise.all([
      api.getGuild(token, guildId.value),
      api.listChannels(token, guildId.value),
      api.listRoles(token, guildId.value),
      api.listMembers(token, guildId.value),
      api.getMe(token),
    ])
    guild.value = guildResp
    channel.value = channels.find((c) => c.id === channelId.value) ?? null
    selfId.value = profile.identity_id
    // Presence isn't needed just to resolve the caller's own permissions,
    // so this fills a placeholder status rather than paying for a
    // getPresence round trip nobody reads here.
    const plainMembers: GuildMember[] = membersResp.map((m) => ({
      identityId: m.identity_id,
      roleIndex: m.role_index,
      status: 'Offline',
      joinedAt: m.joined_at,
    }))
    selfPermissions.value = permissionsForMember(selfId.value, plainMembers, roles)
  }

  async function loadLatestMessages() {
    if (!session.token) return
    const page = await api.listMessages(session.token, guildId.value, channelId.value, {
      limit: MESSAGE_PAGE_SIZE,
    })
    messages.value = toOldestFirst(page)
    hasMoreOlder.value = page.length === MESSAGE_PAGE_SIZE
  }

  async function pollNewMessages() {
    if (!session.token) return
    try {
      const page = await api.listMessages(session.token, guildId.value, channelId.value, {
        limit: MESSAGE_PAGE_SIZE,
      })
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
    if (!session.token || loadingOlder.value || !hasMoreOlder.value || messages.value.length === 0) {
      return
    }
    loadingOlder.value = true
    try {
      const oldestId = messages.value[0].id
      const page = await api.listMessages(session.token, guildId.value, channelId.value, {
        before: oldestId,
        limit: MESSAGE_PAGE_SIZE,
      })
      hasMoreOlder.value = page.length === MESSAGE_PAGE_SIZE
      messages.value = [...toOldestFirst(page), ...messages.value]
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    } finally {
      loadingOlder.value = false
    }
  }

  async function sendMessage(body: string) {
    if (!session.token) return
    sendError.value = ''
    sending.value = true
    try {
      const message = await api.sendMessage(session.token, guildId.value, channelId.value, { body })
      messages.value = [...messages.value, message]
    } catch (e) {
      sendError.value = e instanceof Error ? e.message : 'Something went wrong.'
    } finally {
      sending.value = false
    }
  }

  async function deleteMessage(messageId: string) {
    if (!session.token) return
    error.value = ''
    try {
      await api.deleteMessage(session.token, guildId.value, channelId.value, messageId)
      messages.value = messages.value.filter((m) => m.id !== messageId)
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    }
  }

  async function load() {
    if (!session.token) return
    loading.value = true
    error.value = ''
    try {
      await loadChannelMeta()
      await loadLatestMessages()
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    } finally {
      loading.value = false
    }
  }

  // Same reasoning as useGuildDetail: vue-router reuses the component
  // instance when only :cid/:id change, so a param watch drives reload.
  watch([guildId, channelId], load)

  onMounted(() => {
    load()
    pollHandle = setInterval(pollNewMessages, POLL_INTERVAL_MS)
  })

  onUnmounted(() => {
    if (pollHandle) clearInterval(pollHandle)
  })

  return {
    guild,
    channel,
    messages,
    selfId,
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
  }
}
