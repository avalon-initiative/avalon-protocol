// Integrator directory: GET /integrations behind reactive filters (search text, sort). Mirrors
// useDiscoverGuilds.ts's shape closely — server-side filtering/sorting/
// pagination via the endpoint's own `next_cursor` (crates/server/src/integrations.rs's
// keyset pagination — never re-derived or re-sorted client-side, since only
// the server's `ORDER BY` matches its own cursor comparison) — but this
// endpoint is public and unauthenticated, so unlike useDiscoverGuilds this
// needs no session token at all.
//
// A tier-2 view (a browse list someone might sit on
// for a while) — polls on the same interval useMyConnections/useMyGuilds
// already use, same reasoning: a newly published integration should show
// up without a manual reload. Polling reuses `refresh()` directly rather
// than a separate silent variant, since `loading` here only ever
// suppresses the "no results" empty-state message (never hides already-
// rendered cards), so a poll tick briefly flipping it has no visible
// effect on a page that already has content.
import { onMounted, onUnmounted, ref, watch } from 'vue'
import { listIntegrators } from '@avalon/sdk'
import type { IntegratorSummary } from '@avalon/sdk'
import { buildIntegratorsListQueryString, type ListIntegratorsParams } from '../api/integrations'
import { getServerUrl } from '../api/serverUrl'

const POLL_INTERVAL_MS = 5 * 60_000

export function useDiscoverIntegrations() {
  const query = ref('')
  const sort = ref<'newest' | 'name'>('newest')

  const integrators = ref<IntegratorSummary[]>([])
  const nextCursor = ref<string | null>(null)
  const loading = ref(false)
  const error = ref('')

  let pollHandle: ReturnType<typeof setInterval> | undefined

  function currentParams(cursor?: string): ListIntegratorsParams {
    return {
      q: query.value.trim() || undefined,
      sort: sort.value,
      cursor,
    }
  }

  async function refresh() {
    loading.value = true
    error.value = ''
    try {
      const response = await listIntegrators(getServerUrl(), buildIntegratorsListQueryString(currentParams()))
      integrators.value = response.integrators
      nextCursor.value = response.nextCursor
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    } finally {
      loading.value = false
    }
  }

  async function loadMore() {
    if (!nextCursor.value || loading.value) return
    loading.value = true
    error.value = ''
    try {
      const response = await listIntegrators(getServerUrl(), buildIntegratorsListQueryString(currentParams(nextCursor.value)))
      integrators.value = [...integrators.value, ...response.integrators]
      nextCursor.value = response.nextCursor
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    } finally {
      loading.value = false
    }
  }

  // Re-runs page one whenever a filter changes, same debounce-free
  // milestone-1 stand-in useDiscoverGuilds.ts already takes.
  watch([query, sort], refresh)

  onMounted(async () => {
    await refresh()
    pollHandle = setInterval(refresh, POLL_INTERVAL_MS)
  })

  onUnmounted(() => {
    if (pollHandle) clearInterval(pollHandle)
  })

  return { query, sort, integrators, nextCursor, loading, error, refresh, loadMore }
}
