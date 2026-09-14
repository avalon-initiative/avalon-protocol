// Integrator directory (issue #270): GET /integrations (#293's canonical alias
// for GET /integrations) behind reactive filters (search text, sort). Mirrors
// useDiscoverGuilds.ts's shape closely — server-side filtering/sorting/
// pagination via the endpoint's own `next_cursor` (crates/server/src/integrations.rs's
// keyset pagination — never re-derived or re-sorted client-side, since only
// the server's `ORDER BY` matches its own cursor comparison) — but this
// endpoint is public and unauthenticated, so unlike useDiscoverGuilds this
// needs no session token at all.
import { ref, watch } from 'vue'
import * as api from '../api/client'
import { buildIntegratorsListQueryString } from '../api/integrations'
import type { IntegratorSummary, ListIntegratorsParams } from '../api/types'

export function useDiscoverIntegrations() {
  const query = ref('')
  const sort = ref<'newest' | 'name'>('newest')

  const integrators = ref<IntegratorSummary[]>([])
  const nextCursor = ref<string | null>(null)
  const loading = ref(false)
  const error = ref('')

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
      const response = await api.listIntegrators(buildIntegratorsListQueryString(currentParams()))
      integrators.value = response.integrators
      nextCursor.value = response.next_cursor
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
      const response = await api.listIntegrators(buildIntegratorsListQueryString(currentParams(nextCursor.value)))
      integrators.value = [...integrators.value, ...response.integrators]
      nextCursor.value = response.next_cursor
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    } finally {
      loading.value = false
    }
  }

  // Re-runs page one whenever a filter changes, same debounce-free
  // milestone-1 stand-in useDiscoverGuilds.ts already takes.
  watch([query, sort], refresh)

  return { query, sort, integrators, nextCursor, loading, error, refresh, loadMore }
}
