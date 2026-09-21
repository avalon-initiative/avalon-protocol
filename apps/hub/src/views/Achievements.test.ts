// Issue #389: Achievements.vue used to load once on mount and never
// refresh — this covers that it now polls for updates.
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createMemoryHistory } from 'vue-router'
import { mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import Achievements from './Achievements.vue'
import { useSessionStore } from '../api/session'
import { mockFetchByPath } from '../testing/fakes'

function testRouter() {
  return createRouter({
    history: createMemoryHistory(),
    routes: [{ path: '/', name: 'achievements', component: Achievements }],
  })
}

const profile = {
  identity_id: 'id-self',
  identity_created_at: 'now',
  display_name: 'Nova',
  avatar_url: null,
  discoverable: false,
}

beforeEach(() => {
  localStorage.clear()
  setActivePinia(createPinia())
})

describe('Achievements', () => {
  it('polls GET /me/achievements for updates without a manual reload', async () => {
    vi.useFakeTimers()
    mockFetchByPath({ '/me': profile, '/me/achievements': { achievements: [], next_cursor: null } })
    localStorage.setItem('avalon:session:token', 'a-token')
    await useSessionStore().initialize()

    const router = testRouter()
    router.push('/')
    await router.isReady()
    mount(Achievements, { global: { plugins: [router] } })
    await vi.waitFor(() => {
      const calls = (fetch as ReturnType<typeof vi.fn>).mock.calls as [string][]
      expect(calls.some(([url]) => url.includes('/me/achievements'))).toBe(true)
    })

    ;(fetch as ReturnType<typeof vi.fn>).mockClear()
    await vi.advanceTimersByTimeAsync(15_000)

    const calls = (fetch as ReturnType<typeof vi.fn>).mock.calls as [string][]
    expect(calls.some(([url]) => url.includes('/me/achievements'))).toBe(true)

    vi.useRealTimers()
  })
})
