// Guild discovery board (issue #154): GET /guilds/discover behind reactive
// filters (search text, recruiting-only toggle, tag). Unlike
// useMyGuilds.ts's client-side membership poll, filtering/sorting happens
// server-side — this composable just re-fetches page one whenever a filter
// changes, and appends subsequent pages on `loadMore` using the server's
// `next_cursor` (crates/server/src/guilds.rs's keyset pagination — never
// re-derived or re-sorted client-side, since only the server's `ORDER BY`
// matches its own cursor comparison).
import { ref, watch } from 'vue'
import * as api from '../api/client'
import { buildDiscoverQueryString } from '../api/guilds'
import type { DiscoverGuildSummary, DiscoverGuildsParams } from '../api/types'
import { useSessionStore } from '../stores/session'

export function useDiscoverGuilds() {
  const session = useSessionStore()

  const query = ref('')
  const recruitingOnly = ref(true)
  const tag = ref('')

  const guilds = ref<DiscoverGuildSummary[]>([])
  const nextCursor = ref<string | null>(null)
  const loading = ref(false)
  const error = ref('')

  function currentParams(cursor?: string): DiscoverGuildsParams {
    return {
      q: query.value.trim() || undefined,
      recruiting: recruitingOnly.value ? true : undefined,
      tag: tag.value.trim() || undefined,
      cursor,
    }
  }

  async function refresh() {
    if (!session.token) return
    loading.value = true
    error.value = ''
    try {
      const response = await api.discoverGuilds(session.token, buildDiscoverQueryString(currentParams()))
      guilds.value = response.guilds
      nextCursor.value = response.next_cursor
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    } finally {
      loading.value = false
    }
  }

  async function loadMore() {
    if (!session.token || !nextCursor.value || loading.value) return
    loading.value = true
    error.value = ''
    try {
      const response = await api.discoverGuilds(
        session.token,
        buildDiscoverQueryString(currentParams(nextCursor.value)),
      )
      guilds.value = [...guilds.value, ...response.guilds]
      nextCursor.value = response.next_cursor
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    } finally {
      loading.value = false
    }
  }

  // Re-runs page one whenever a filter changes — the search box included,
  // debounce-free for now (milestone-1 stand-in, same posture as the
  // endpoint itself); a real read model (#42) is the point to revisit
  // request-shaping like debouncing too.
  watch([query, recruitingOnly, tag], refresh)

  return { query, recruitingOnly, tag, guilds, nextCursor, loading, error, refresh, loadMore }
}
