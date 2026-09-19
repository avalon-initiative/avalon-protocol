// Issue #389: "people you may know" used to load once on mount and never
// refresh.
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createMemoryHistory } from 'vue-router'
import { mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import Friends from './Friends.vue'
import { useSessionStore } from '@avalon/api-client'
import { FakeWebSocket, mockFetchByPath } from '../testing/fakes'

function testRouter() {
  return createRouter({
    history: createMemoryHistory(),
    routes: [{ path: '/', name: 'friends', component: Friends }],
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
  vi.stubGlobal('WebSocket', FakeWebSocket)
})

describe('Friends "people you may know" polling', () => {
  it('picks up a new suggestion without a manual reload', async () => {
    useSessionStore().login('a-token')
    vi.useFakeTimers()
    mockFetchByPath({
      '/me': profile,
      '/friends': [],
      '/friends/requests': [],
      '/presence': [],
      '/people/discover': { candidates: [] },
    })

    const router = testRouter()
    router.push('/')
    await router.isReady()
    const wrapper = mount(Friends, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Friends'))

    mockFetchByPath({
      '/me': profile,
      '/friends': [],
      '/friends/requests': [],
      '/presence': [],
      '/people/discover': { candidates: [{ identity_id: 'id-suggested' }] },
      '/identities/profiles': [
        { identity_id: 'id-suggested', display_name: 'Ilya', avatar_url: null },
      ],
    })
    await vi.advanceTimersByTimeAsync(15_000)

    expect(wrapper.text()).toContain('Ilya')
    vi.useRealTimers()
  })
})
