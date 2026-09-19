// Home page "Latest Messages" panel (issue #312): the most recent message
// per guild the caller belongs to, newest guild-message first. Only the
// guild's first (oldest-created) non-archived channel is checked per
// guild — a full cross-channel merge is more than a Home summary widget
// needs; Guild.vue's own channel list is where that already happens.
import { onUnmounted, ref, watch, type Ref } from 'vue'
import * as api from '@avalon/api-client'
import type { GuildResponse, MessageResponse } from '@avalon/api-client'
import { useSessionStore } from '@avalon/api-client'

const POLL_INTERVAL_MS = 5 * 60_000

export interface LatestGuildMessage {
  guildId: string
  guildName: string
  channelId: string
  message: MessageResponse
}

// Takes the caller's already-loaded guilds as a Ref (from useMyGuilds)
// rather than fetching its own copy — this composable only adds the
// per-guild "latest message" lookup on top.
export function useLatestGuildMessages(guilds: Ref<GuildResponse[]>) {
  const session = useSessionStore()

  const latestMessages = ref<LatestGuildMessage[]>([])
  const loading = ref(true)
  const error = ref('')

  async function latestForGuild(token: string, guild: GuildResponse): Promise<LatestGuildMessage | null> {
    const channels = await api.listChannels(token, guild.id)
    const channel = channels.find((c) => !c.archived)
    if (!channel) return null

    const messages = await api.listMessages(token, guild.id, channel.id, { limit: 1 })
    const message = messages[0]
    if (!message) return null

    return { guildId: guild.id, guildName: guild.name, channelId: channel.id, message }
  }

  async function refresh() {
    if (!session.token) return
    try {
      const token = session.token
      const results = await Promise.all(guilds.value.map((g) => latestForGuild(token, g)))
      latestMessages.value = results
        .filter((r): r is LatestGuildMessage => r !== null)
        .sort((a, b) => b.message.sent_at.localeCompare(a.message.sent_at))
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    } finally {
      loading.value = false
    }
  }

  const stopWatch = watch(guilds, refresh, { immediate: true })
  const pollHandle = setInterval(refresh, POLL_INTERVAL_MS)

  onUnmounted(() => {
    stopWatch()
    if (pollHandle) clearInterval(pollHandle)
  })

  return { latestMessages, loading, error, refresh }
}
