// Channel chat, embedded in Guild.vue's Channels tab (issue #22/#24, later
// folded out of the standalone GuildChannel.vue route by #241): cursor-
// paginated history (`before`/`limit`, matching
// crates/server/src/guild_messages.rs), newest at the bottom, load-older on
// demand. New messages arrive live over the /ws/messages socket (issue
// #438) rather than a poll — a channel that used to be milestone-1 poll-only
// per its own now-outdated design note; see docs/architecture/communication.md.
//
// `channelId` is expected to change while this composable stays mounted —
// the Channels tab sidebar swaps it as the reader picks a different channel,
// with no route navigation or component remount in between (#241). An empty
// `channelId` (no channel selected/available yet) is a valid, quiet state,
// not an error: every loader below short-circuits on it rather than hitting
// the API with a malformed URL.
import { computed, onMounted, onUnmounted, ref, watch, type Ref } from 'vue'
import * as api from '../api/client'
import { hasGuildPermission, permissionsForMember } from '../api/guilds'
import type { GuildMember } from '../api/guilds'
import { toOldestFirst } from '../api/guildChat'
import type { ArchivedMessageResponse, ChannelResponse, GuildResponse, MessageResponse } from '../api/types'
import { useSessionStore } from '../stores/session'

const MESSAGE_PAGE_SIZE = 50

// Issue #464. A live message and an archived one render identically except
// for this flag — archived history is read-only (no live-update, no
// delete), never merged with the live table server-side.
export interface ChatMessage extends MessageResponse {
  archived?: boolean
}

function fromArchive(messages: ArchivedMessageResponse[]): ChatMessage[] {
  return messages.map((m) => ({
    id: m.id,
    channel_id: m.channel_id,
    author: m.author,
    body: m.body,
    sent_at: m.sent_at,
    archived: true,
  }))
}

export function useGuildChat(guildId: Ref<string>, channelId: Ref<string>) {
  const session = useSessionStore()

  const guild = ref<GuildResponse | null>(null)
  const channel = ref<ChannelResponse | null>(null)
  const messages = ref<ChatMessage[]>([]) // oldest-first, for newest-at-bottom rendering
  // Issue #464. Once the live table's before-cursor pagination is
  // exhausted for this channel, further "load older" calls read the
  // archive tier instead — flipped inside loadOlder, reset by load()
  // whenever the channel changes.
  const readingArchive = ref(false)
  // identity id -> "display_name#discriminator", resolved via
  // GET /identities/profiles (issue #161) for whichever authors show up
  // in the currently-loaded messages. Never removed once resolved — an
  // author's name doesn't need to change mid-session for a chat view.
  const authorNames = ref<Record<string, string>>({})
  const selfId = ref('')
  const selfPermissions = ref<string[]>([])
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

  // Every async operation below reads channelId.value again after its
  // awaited work returns and bails if it's changed — the reader may have
  // picked a different channel from the sidebar while a request for the
  // old one was still in flight (no route navigation/remount guards this
  // for us anymore, per #241's design). Without this, a slow response for
  // a channel the reader already navigated away from could still land and
  // overwrite `messages`/`channel` with stale data.
  function isStaleFor(targetChannelId: string): boolean {
    return channelId.value !== targetChannelId
  }

  const canDelete = computed(
    () =>
      guild.value !== null &&
      hasGuildPermission(guild.value, selfId.value, selfPermissions.value, 'manage_channels'),
  )

  async function loadChannelMeta(targetChannelId: string) {
    if (!session.token || !targetChannelId) return
    const token = session.token
    const [guildResp, channels, roles, membersResp, profile] = await Promise.all([
      api.getGuild(token, guildId.value),
      api.listChannels(token, guildId.value),
      api.listRoles(token, guildId.value),
      api.listMembers(token, guildId.value),
      api.getMe(token),
    ])
    if (isStaleFor(targetChannelId)) return
    guild.value = guildResp
    channel.value = channels.find((c) => c.id === targetChannelId) ?? null
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

  async function resolveAuthorNames(newMessages: MessageResponse[]) {
    if (!session.token) return
    const unknown = [...new Set(newMessages.map((m) => m.author))].filter(
      (id) => !(id in authorNames.value),
    )
    if (unknown.length === 0) return
    try {
      const profiles = await api.getProfiles(session.token, unknown)
      const resolved: Record<string, string> = {}
      for (const profile of profiles) {
        resolved[profile.identity_id] = `${profile.display_name}#${profile.discriminator}`
      }
      authorNames.value = { ...authorNames.value, ...resolved }
    } catch {
      // Best-effort — messages still render with the raw author id.
    }
  }

  async function loadLatestMessages(targetChannelId: string) {
    if (!session.token || !targetChannelId) return
    const page = await api.listMessages(session.token, guildId.value, targetChannelId, {
      limit: MESSAGE_PAGE_SIZE,
    })
    if (isStaleFor(targetChannelId)) return
    readingArchive.value = false
    messages.value = toOldestFirst(page)
    // A full page definitely means more live history remains. A shorter
    // page (including empty) means the live table is exhausted for this
    // channel, but the archive tier (#253/#464) might still hold older
    // history — stays true so a scroll can find out via loadOlder's own
    // live/archive fallthrough below, rather than assuming "no more"
    // from the live table alone.
    hasMoreOlder.value = true
    await resolveAuthorNames(messages.value)
  }

  // Opens the live socket for `targetChannelId` (issue #438) — closes
  // whatever was subscribed before, so a channel switch never leaves a
  // stale connection pushing updates for a channel the reader left.
  function subscribeToChannel(targetChannelId: string) {
    closeChatSocket()
    if (!session.token || !targetChannelId) return
    chatSocket = api.openChannelMessageSocket(
      session.token,
      guildId.value,
      targetChannelId,
      (message) => {
        if (isStaleFor(targetChannelId)) return
        if (messages.value.some((m) => m.id === message.id)) return
        messages.value = [...messages.value, message]
        resolveAuthorNames([message])
      },
      (messageId) => {
        if (isStaleFor(targetChannelId)) return
        messages.value = messages.value.filter((m) => m.id !== messageId)
      },
    )
  }

  async function loadOlder() {
    const targetChannelId = channelId.value
    if (!session.token || !targetChannelId || loadingOlder.value || !hasMoreOlder.value) {
      return
    }
    loadingOlder.value = true
    try {
      // Undefined when there are no live messages loaded at all — a
      // channel whose entire current history has already aged into the
      // archive tier (issue #253/#464). That's still a valid state to
      // page from: it just means the very next read has to go straight
      // to the archive with no cursor, same as the live-exhausted case
      // below.
      const oldestId = messages.value[0]?.id
      const alreadyReadingArchive = readingArchive.value

      if (alreadyReadingArchive) {
        // oldestId is guaranteed to be an archived message's own id here
        // (list_archive's before-cursor subquery looks it up in
        // guild_messages_archive itself, not the live table).
        const archivePage = await api.getMessageArchive(session.token, guildId.value, targetChannelId, {
          before: oldestId,
          limit: MESSAGE_PAGE_SIZE,
        })
        if (isStaleFor(targetChannelId)) return
        hasMoreOlder.value = archivePage.length === MESSAGE_PAGE_SIZE
        const older = toOldestFirst(fromArchive(archivePage))
        messages.value = [...older, ...messages.value]
        await resolveAuthorNames(older)
        return
      }

      if (oldestId) {
        const page = await api.listMessages(session.token, guildId.value, targetChannelId, {
          before: oldestId,
          limit: MESSAGE_PAGE_SIZE,
        })
        if (isStaleFor(targetChannelId)) return

        if (page.length === MESSAGE_PAGE_SIZE) {
          // A full page means more live history may remain — no need to
          // touch the archive tier yet.
          hasMoreOlder.value = true
          const older = toOldestFirst(page)
          messages.value = [...older, ...messages.value]
          await resolveAuthorNames(older)
          return
        }

        const liveOlder = toOldestFirst(page)
        messages.value = [...liveOlder, ...messages.value]
        await resolveAuthorNames(liveOlder)
      }

      // Either the live table's before-cursor pagination just came back
      // short of a full page (live history exhausted for this channel),
      // or there was no live message to page from in the first place —
      // either way, the cap-pruned tail of this channel's history, if
      // any, lives in the archive tier instead. Continuing into it within
      // this same call, rather than waiting for a future click that
      // hasMoreOlder might otherwise never allow. Starts fresh with no
      // `before`: oldestId, if set, is a live-table id and can't be
      // reused as the archive's own cursor.
      readingArchive.value = true
      const archivePage = await api.getMessageArchive(session.token, guildId.value, targetChannelId, {
        limit: MESSAGE_PAGE_SIZE,
      })
      if (isStaleFor(targetChannelId)) return
      hasMoreOlder.value = archivePage.length === MESSAGE_PAGE_SIZE
      const archiveOlder = toOldestFirst(fromArchive(archivePage))
      messages.value = [...archiveOlder, ...messages.value]
      await resolveAuthorNames(archiveOlder)
    } catch (e) {
      if (!isStaleFor(targetChannelId)) {
        error.value = e instanceof Error ? e.message : 'Something went wrong.'
      }
    } finally {
      loadingOlder.value = false
    }
  }

  async function sendMessage(body: string) {
    const targetChannelId = channelId.value
    if (!session.token || !targetChannelId) return
    sendError.value = ''
    sending.value = true
    try {
      const message = await api.sendMessage(session.token, guildId.value, targetChannelId, { body })
      // The websocket push for this same message can arrive before this
      // response does (the server broadcasts right after the DB insert,
      // before this HTTP round trip completes) — dedup against it, same
      // guard subscribeToChannel's onMessage uses.
      if (!isStaleFor(targetChannelId) && !messages.value.some((m) => m.id === message.id)) {
        messages.value = [...messages.value, message]
        await resolveAuthorNames([message])
      }
    } catch (e) {
      if (!isStaleFor(targetChannelId)) {
        sendError.value = e instanceof Error ? e.message : 'Something went wrong.'
      }
    } finally {
      sending.value = false
    }
  }

  async function deleteMessage(messageId: string) {
    const targetChannelId = channelId.value
    if (!session.token || !targetChannelId) return
    error.value = ''
    try {
      await api.deleteMessage(session.token, guildId.value, targetChannelId, messageId)
      if (!isStaleFor(targetChannelId)) {
        messages.value = messages.value.filter((m) => m.id !== messageId)
      }
    } catch (e) {
      if (!isStaleFor(targetChannelId)) {
        error.value = e instanceof Error ? e.message : 'Something went wrong.'
      }
    }
  }

  async function load() {
    const targetChannelId = channelId.value
    if (!session.token) return
    if (!targetChannelId) {
      // Nothing selected yet (e.g. the Channels tab hasn't picked a
      // channel, or the guild has none) — a quiet empty state, not a load
      // in progress.
      messages.value = []
      readingArchive.value = false
      channel.value = null
      loading.value = false
      closeChatSocket()
      return
    }
    loading.value = true
    error.value = ''
    try {
      await loadChannelMeta(targetChannelId)
      if (isStaleFor(targetChannelId)) return
      await loadLatestMessages(targetChannelId)
      if (isStaleFor(targetChannelId)) return
      subscribeToChannel(targetChannelId)
    } catch (e) {
      if (!isStaleFor(targetChannelId)) {
        error.value = e instanceof Error ? e.message : 'Something went wrong.'
      }
    } finally {
      if (!isStaleFor(targetChannelId)) {
        loading.value = false
      }
    }
  }

  // Same reasoning as useGuildDetail: vue-router reuses the component
  // instance when only :cid/:id change, so a param watch drives reload —
  // and #241's Channels tab swaps `channelId` itself with no route change
  // at all when the reader picks a different channel from the sidebar.
  watch([guildId, channelId], load)

  onMounted(load)

  onUnmounted(() => {
    closeChatSocket()
  })

  return {
    guild,
    channel,
    messages,
    authorNames,
    selfId,
    canDelete,
    loading,
    loadingOlder,
    hasMoreOlder,
    readingArchive,
    error,
    sendError,
    sending,
    loadOlder,
    sendMessage,
    deleteMessage,
  }
}
