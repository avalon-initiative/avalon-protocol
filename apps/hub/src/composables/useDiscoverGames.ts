// Game directory (issue #270): GET /games behind reactive filters (search
// text, sort). Mirrors useDiscoverGuilds.ts's shape closely — server-side
// filtering/sorting/pagination via the endpoint's own `next_cursor`
// (crates/server/src/games.rs's keyset pagination — never re-derived or
// re-sorted client-side, since only the server's `ORDER BY` matches its
// own cursor comparison) — but GET /games is public and unauthenticated,
// so unlike useDiscoverGuilds this needs no session token at all.
import { ref, watch } from 'vue'
import * as api from '../api/client'
import { buildGamesListQueryString } from '../api/games'
import type { GameSummary, ListGamesParams } from '../api/types'

export function useDiscoverGames() {
  const query = ref('')
  const sort = ref<'newest' | 'name'>('newest')

  const games = ref<GameSummary[]>([])
  const nextCursor = ref<string | null>(null)
  const loading = ref(false)
  const error = ref('')

  function currentParams(cursor?: string): ListGamesParams {
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
      const response = await api.listGames(buildGamesListQueryString(currentParams()))
      games.value = response.games
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
      const response = await api.listGames(buildGamesListQueryString(currentParams(nextCursor.value)))
      games.value = [...games.value, ...response.games]
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

  return { query, sort, games, nextCursor, loading, error, refresh, loadMore }
}
