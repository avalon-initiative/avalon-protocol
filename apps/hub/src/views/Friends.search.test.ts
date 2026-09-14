// Issue #205's user search UI on the Friends page — a search box +
// results list reachable alongside the existing #204 "people you may
// know" suggestions, using GET /identities/search.
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createMemoryHistory } from 'vue-router'
import { mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import Friends from './Friends.vue'
import { useSessionStore } from '../stores/session'
import { FakeWebSocket, mockFetchByPath } from '../testing/fakes'

function testRouter() {
  return createRouter({
    history: createMemoryHistory(),
    routes: [{ path: '/', name: 'friends', component: Friends }],
  })
}

const profile = {
  identity_id: 'id-1',
  identity_created_at: 'now',
  display_name: 'Nova',
  avatar_url: null,
  handle: 'Nova#4821',
  discoverable: false,
}

beforeEach(() => {
  localStorage.clear()
  setActivePinia(createPinia())
  vi.stubGlobal('WebSocket', FakeWebSocket)
})

async function mountFriends(extraResponses: Record<string, unknown> = {}) {
  useSessionStore().login('a-token')
  mockFetchByPath({
    '/me': profile,
    '/friends': [],
    '/friends/requests': [],
    '/presence': [],
    '/people/discover': { candidates: [] },
    ...extraResponses,
  })

  const router = testRouter()
  router.push('/')
  await router.isReady()
  const wrapper = mount(Friends, { global: { plugins: [router] } })
  await vi.waitFor(() => expect(wrapper.text()).toContain('Search for users'))
  return wrapper
}

describe('Friends user search (issue #205)', () => {
  it('shows a search box that is reachable without any prior friend/suggestion state', async () => {
    const wrapper = await mountFriends()
    expect(wrapper.find('input').exists()).toBe(true)
  })

  it('shows matching opted-in results after searching', async () => {
    const wrapper = await mountFriends({
      '/identities/search': {
        results: [
          { identity_id: 'other-1', display_name: 'Alice', discriminator: '1234', avatar_url: null },
        ],
      },
    })

    // Two forms exist on this page — "Add a friend" (first) and "Search
    // for users" (second) — so the search form has to be targeted
    // specifically rather than grabbing the first `<form>` on the page.
    await wrapper.findAll('input')[1].setValue('alice')
    await wrapper.findAll('form')[1].trigger('submit')

    await vi.waitFor(() => expect(wrapper.text()).toContain('Alice'))
  })

  it('shows an empty-results message when nothing publicly searchable matches', async () => {
    const wrapper = await mountFriends({
      '/identities/search': { results: [] },
    })

    await wrapper.findAll('input')[1].setValue('nobody-like-this')
    await wrapper.findAll('form')[1].trigger('submit')

    await vi.waitFor(() =>
      expect(wrapper.text()).toContain('No publicly searchable users match that.'),
    )
  })
})
