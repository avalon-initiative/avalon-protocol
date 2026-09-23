// Guild discovery board: GET /guilds/discover behind reactive
// filters (search text, recruiting-only toggle, tag). Unlike
// useMyGuilds.ts's client-side membership poll, filtering/sorting happens
// server-side — this composable just re-fetches page one whenever a filter
// changes, and appends subsequent pages on `loadMore` using the server's
// `next_cursor` (crates/server/src/guilds.rs's keyset pagination — never
// re-derived or re-sorted client-side, since only the server's `ORDER BY`
// matches its own cursor comparison).
//
// A tier-2 view — polls once activated, same interval
// useMyConnections/useMyGuilds already use. Unlike those composables this
// one doesn't fetch on mount: Guilds.vue's Discover tab is lazy (only
// loaded the first time a reader actually opens it), so polling only
// starts the first time `refresh()` actually runs, not unconditionally —
// no point polling a board nobody has opened yet.
import { onUnmounted, ref, watch } from 'vue'
import { buildDiscoverQueryString } from '../api/guilds'
import type { DiscoverGuildsParams } from '../api/guilds'
import type { DiscoverGuildSummary } from '@avalon/sdk'
import { useSessionStore } from '../api/session'

const POLL_INTERVAL_MS = 5 * 60_000

export function useDiscoverGuilds() {
  const session = useSessionStore()

  let pollHandle: ReturnType<typeof setInterval> | undefined

  const query = ref('')
  const recruitingOnly = ref(true)
  const tag = ref('')
  // Issue #467: pre-filters to guilds associated with one integrator,
  // set from IntegrationProfile.vue's "Guilds playing this" link
  // (?integrator=<slug> on the Discover tab's route).
  const integratorSlug = ref('')

  const guilds = ref<DiscoverGuildSummary[]>([])
  const nextCursor = ref<string | null>(null)
  const loading = ref(false)
  const error = ref('')

  function currentParams(cursor?: string): DiscoverGuildsParams {
    return {
      q: query.value.trim() || undefined,
      recruiting: recruitingOnly.value ? true : undefined,
      tag: tag.value.trim() || undefined,
      integrator: integratorSlug.value.trim() || undefined,
      cursor,
    }
  }

  async function refresh() {
    const s = session.session
    if (!s) return
    loading.value = true
    error.value = ''
    try {
      const response = await s.discoverGuilds(buildDiscoverQueryString(currentParams()))
      guilds.value = Array.isArray(response.guilds) ? response.guilds : []
      nextCursor.value = response.nextCursor
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    } finally {
      loading.value = false
    }
    if (!pollHandle) {
      pollHandle = setInterval(refresh, POLL_INTERVAL_MS)
    }
  }

  async function loadMore() {
    const s = session.session
    if (!s || !nextCursor.value || loading.value) return
    loading.value = true
    error.value = ''
    try {
      const response = await s.discoverGuilds(buildDiscoverQueryString(currentParams(nextCursor.value)))
      guilds.value = [...guilds.value, ...(Array.isArray(response.guilds) ? response.guilds : [])]
      nextCursor.value = response.nextCursor
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    } finally {
      loading.value = false
    }
  }

  // Re-runs page one whenever a filter changes — the search box included,
  // debounce-free for now (milestone-1 stand-in, same posture as the
  // endpoint itself); a real read model is the point to revisit
  // request-shaping like debouncing too.
  watch([query, recruitingOnly, tag, integratorSlug], refresh)

  onUnmounted(() => {
    if (pollHandle) clearInterval(pollHandle)
  })

  return {
    query,
    recruitingOnly,
    tag,
    integratorSlug,
    guilds,
    nextCursor,
    loading,
    error,
    refresh,
    loadMore,
  }
}
