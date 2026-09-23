// Issue #442: the pending guild invites the caller has received, surfaced
// on Guilds.vue so an invitee actually sees an invite exists instead of
// needing its raw id shared out of band. Polled like useMyGuilds — an
// invite showing up a little late is fine, this isn't a tier-1 realtime
// need.
import { onMounted, onUnmounted, ref } from 'vue'
import type { MyGuildInvite } from '@avalon-initiative/protocol-sdk'
import { useSessionStore } from '../api/session'

const POLL_INTERVAL_MS = 5 * 60_000

export function useMyGuildInvites() {
  const session = useSessionStore()

  const invites = ref<MyGuildInvite[]>([])
  const inviterNames = ref<Record<string, string>>({})
  const loading = ref(true)
  const error = ref('')

  async function resolveInviterNames(rows: MyGuildInvite[]) {
    const s = session.session
    if (!s) return
    const unknown = [...new Set(rows.map((r) => r.from))].filter(
      (id) => !(id in inviterNames.value),
    )
    if (unknown.length === 0) return
    try {
      const profiles = await s.profiles(unknown)
      const resolved: Record<string, string> = {}
      for (const profile of Array.isArray(profiles) ? profiles : []) {
        resolved[profile.identityId] = profile.displayName
      }
      inviterNames.value = { ...inviterNames.value, ...resolved }
    } catch {
      // Best-effort — invites still render with the raw id.
    }
  }

  let pollHandle: ReturnType<typeof setInterval> | undefined

  async function refresh() {
    const s = session.session
    if (!s) return
    try {
      const rows = await s.myGuildInvites()
      invites.value = Array.isArray(rows) ? rows : []
      await resolveInviterNames(invites.value)
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

  return { invites, inviterNames, loading, error, refresh }
}
