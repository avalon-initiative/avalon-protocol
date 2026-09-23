// Home page "Latest Messages" panel: the most recent message
// per guild the caller belongs to, newest guild-message first. Only the
// guild's first (oldest-created) non-archived channel is checked per
// guild — a full cross-channel merge is more than a Home summary widget
// needs; Guild.vue's own channel list is where that already happens.
import { onUnmounted, ref, watch, type Ref } from 'vue'
import type { AccountSession, Guild, GuildMessage } from '@avalon/sdk'
import { useSessionStore } from '../api/session'

const POLL_INTERVAL_MS = 5 * 60_000

export interface LatestGuildMessage {
  guildId: string
  guildName: string
  channelId: string
  message: GuildMessage
}

// Takes the caller's already-loaded guilds as a Ref (from useMyGuilds)
// rather than fetching its own copy — this composable only adds the
// per-guild "latest message" lookup on top.
export function useLatestGuildMessages(guilds: Ref<Guild[]>) {
  const session = useSessionStore()

  const latestMessages = ref<LatestGuildMessage[]>([])
  const loading = ref(true)
  const error = ref('')

  async function latestForGuild(s: AccountSession, guild: Guild): Promise<LatestGuildMessage | null> {
    const channels = await s.listChannels(guild.id)
    const channel = Array.isArray(channels) ? channels.find((c) => !c.archived) : undefined
    if (!channel) return null

    const messages = await s.channelMessages(guild.id, channel.id, undefined, 1)
    const message = Array.isArray(messages) ? messages[0] : undefined
    if (!message) return null

    return { guildId: guild.id, guildName: guild.name, channelId: channel.id, message }
  }

  async function refresh() {
    const s = session.session
    if (!s) return
    try {
      const results = await Promise.all(guilds.value.map((g) => latestForGuild(s, g)))
      latestMessages.value = results
        .filter((r): r is LatestGuildMessage => r !== null)
        .sort((a, b) => b.message.sentAt.localeCompare(a.message.sentAt))
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
