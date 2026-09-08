// "My guilds" list for the Guilds.vue landing page (issue #24). GET
// /me/guilds only returns { guild_id, role_index, joined_at } — guild
// name/tag/description/member_count come from a separate GET /guilds/{id}
// per membership, same "the membership endpoint doesn't embed the thing
// the UI needs, fetch it separately" shape as friends/presence.
import { onMounted, onUnmounted, ref } from 'vue'
import * as api from '../api/client'
import type { GuildResponse } from '../api/types'
import { useSessionStore } from '../stores/session'

// Guild metadata (rename, member count) isn't push-updated anywhere in
// this build, same as friends' membership poll — 5 minutes is plenty.
const POLL_INTERVAL_MS = 5 * 60_000

export function useMyGuilds() {
  const session = useSessionStore()

  const guilds = ref<GuildResponse[]>([])
  const loading = ref(true)
  const error = ref('')

  let pollHandle: ReturnType<typeof setInterval> | undefined

  async function refresh() {
    if (!session.token) return
    try {
      const memberships = await api.listMyGuilds(session.token)
      const token = session.token
      guilds.value = await Promise.all(memberships.map((m) => api.getGuild(token, m.guild_id)))
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    }
  }

  onMounted(async () => {
    await refresh()
    loading.value = false
    pollHandle = setInterval(refresh, POLL_INTERVAL_MS)
  })

  onUnmounted(() => {
    if (pollHandle) clearInterval(pollHandle)
  })

  return { guilds, loading, error, refresh }
}
