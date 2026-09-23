// "My guilds" list for the Guilds.vue landing page. GET
// /me/guilds only returns { guildId, roleIndex, joinedAt } — guild
// name/tag/description/memberCount come from a separate GET /guilds/{id}
// per membership, same "the membership endpoint doesn't embed the thing
// the UI needs, fetch it separately" shape as friends/presence.
import { onMounted, onUnmounted, ref } from 'vue'
import type { Guild } from '@avalon/sdk'
import { useSessionStore } from '../api/session'

// Guild metadata (rename, member count) isn't push-updated anywhere in
// this build, same as friends' membership poll — 5 minutes is plenty.
const POLL_INTERVAL_MS = 5 * 60_000

export function useMyGuilds() {
  const session = useSessionStore()

  const guilds = ref<Guild[]>([])
  const loading = ref(true)
  const error = ref('')

  let pollHandle: ReturnType<typeof setInterval> | undefined

  async function refresh() {
    const s = session.session
    if (!s) return
    try {
      const memberships = await s.myGuilds()
      if (!Array.isArray(memberships)) return
      guilds.value = await Promise.all(memberships.map((m) => s.getGuild(m.guildId)))
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
