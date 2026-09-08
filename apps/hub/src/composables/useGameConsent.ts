// Game-connect consent view (#27) for a given slug: loads the game's
// public registration info once (no polling needed — a game's declared
// capabilities don't change while a player is looking at the consent
// screen), and owns the checked-capabilities set the view submits.
import { ref, type Ref } from 'vue'
import * as api from '../api/client'
import type { GameResponse } from '../api/types'
import { useSessionStore } from '../stores/session'

export function useGameConsent(slug: Ref<string>) {
  const session = useSessionStore()

  const game = ref<GameResponse | null>(null)
  const loading = ref(true)
  const error = ref('')

  // Unchecked by default (the ticket's own invariant) — starts empty
  // regardless of what the game requested.
  const checkedCapabilities = ref<Set<string>>(new Set())

  async function load() {
    if (!session.token) return
    loading.value = true
    error.value = ''
    try {
      game.value = await api.getGame(session.token, slug.value)
      checkedCapabilities.value = new Set()
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Something went wrong.'
    } finally {
      loading.value = false
    }
  }

  function setCapabilityChecked(capability: string, checked: boolean) {
    const next = new Set(checkedCapabilities.value)
    if (checked) {
      next.add(capability)
    } else {
      next.delete(capability)
    }
    checkedCapabilities.value = next
  }

  return { game, loading, error, checkedCapabilities, setCapabilityChecked, load }
}
