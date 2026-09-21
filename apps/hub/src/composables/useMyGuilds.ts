// "My guilds" list for the Guilds.vue landing page (issue #24). GET
// /me/guilds only returns { guild_id, role_index, joined_at } — guild
// name/tag/description/member_count come from a separate GET /guilds/{id}
// per membership, same "the membership endpoint doesn't embed the thing
// the UI needs, fetch it separately" shape as friends/presence.
import { onMounted, onUnmounted, ref } from 'vue'
import * as api from '@avalon/api-client'
import type { GuildResponse } from '@avalon/api-client'
// #712: still on @avalon/api-client for the actual guild reads (that's
// the guilds/chat/events migration batch, not this one) — only the token
// source moved, since production login (Login.vue/CreateIdentity.vue/
// RecoverIdentity.vue) only ever populates the new session store now, and
// this composable's own bearer token needed to keep working against it.
import { useSessionStore } from '../api/session'

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
    const token = session.token()
    if (!token) return
    try {
      const memberships = await api.listMyGuilds(token)
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
