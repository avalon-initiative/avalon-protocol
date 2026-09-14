// Connected-integrators view (#27, #83): every binding the caller has, with its
// currently-active grants. Loads once, then polls — same "no WebSocket
// needed for milestone 1" shape apps/hub/src/composables/useGuildDetail.ts
// already establishes.
import { onMounted, onUnmounted, ref } from 'vue'
import * as api from '../api/client'
import type { IntegratorBindingResponse } from '../api/types'
import { useSessionStore } from '../stores/session'

const POLL_INTERVAL_MS = 5 * 60_000

export function useMyConnections() {
  const session = useSessionStore()

  const bindings = ref<IntegratorBindingResponse[]>([])
  const loading = ref(true)
  const error = ref('')

  let pollHandle: ReturnType<typeof setInterval> | undefined

  async function refresh() {
    if (!session.token) return
    try {
      // GET /me/connections returns a bare array (only active bindings are
      // ever listed, so there's no wrapper object to unwrap).
      bindings.value = await api.listMyConnections(session.token)
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

  return { bindings, loading, error, refresh }
}
