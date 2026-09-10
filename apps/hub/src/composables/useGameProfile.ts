// Per-game profile page (issue #270, first slice of #90): GET
// /integrations/{slug} (#293's canonical alias for GET /games/{slug},
// public fields) + GET /games/{slug}/registry (#261's five class-labeled
// metrics — not renamed by #293), fetched together since the profile page
// always needs both. Both reads are public/unauthenticated — no session
// token.
import { computed, ref, watch } from 'vue'
import type { Ref } from 'vue'
import * as api from '../api/client'
import { listRegistryMetrics } from '../api/games'
import type { GameResponse } from '../api/types'

export function useGameProfile(slug: Ref<string>) {
  const game = ref<GameResponse | null>(null)
  const registry = ref<Awaited<ReturnType<typeof api.getGameRegistry>> | null>(null)
  const metrics = computed(() => (registry.value ? listRegistryMetrics(registry.value) : []))
  const loading = ref(true)
  const error = ref('')

  async function load() {
    loading.value = true
    error.value = ''
    try {
      const [gameResponse, registryResponse] = await Promise.all([
        api.getGamePublic(slug.value),
        api.getGameRegistry(slug.value),
      ])
      game.value = gameResponse
      registry.value = registryResponse
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    } finally {
      loading.value = false
    }
  }

  watch(slug, load, { immediate: true })

  return { game, registry, metrics, loading, error, load }
}
