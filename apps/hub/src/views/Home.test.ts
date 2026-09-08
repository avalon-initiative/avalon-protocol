// The landing page (issue #148): a welcome header from GET /me, and
// sensible empty states when a fresh identity has no friends or history.
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createMemoryHistory } from 'vue-router'
import { mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import Home from './Home.vue'
import { useSessionStore } from '../stores/session'
import { FakeWebSocket, mockFetchByPath } from '../testing/fakes'

const profile = {
  identity_id: 'id-1',
  identity_created_at: 'now',
  display_name: 'Nova',
  avatar_url: null,
  handle: 'Nova#4821',
}

function testRouter() {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', component: Home },
      { path: '/friends', component: Home },
      { path: '/activity', component: Home },
      { path: '/profile', component: Home },
    ],
  })
}

beforeEach(() => {
  localStorage.clear()
  setActivePinia(createPinia())
  vi.stubGlobal('WebSocket', FakeWebSocket)
})

describe('Home', () => {
  it('welcomes the player by display name', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath({ '/me': profile, '/me/history': [], '/friends': [], '/friends/requests': [], '/presence': [] })

    const router = testRouter()
    router.push('/')
    await router.isReady()
    const wrapper = mount(Home, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Welcome back, Nova'))
  })

  it('renders sensible empty states with no friends and no history', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath({ '/me': profile, '/me/history': [], '/friends': [], '/friends/requests': [], '/presence': [] })

    const router = testRouter()
    router.push('/')
    await router.isReady()
    const wrapper = mount(Home, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('No activity yet'))
    expect(wrapper.text()).toContain('No friends online right now.')
    expect(wrapper.text()).toContain('Quick Actions')
  })

  it('shows recent activity summaries and a View all link', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath({
      '/me': profile,
      '/me/history': [
        {
          event_id: 'evt-1',
          kind: 'identity.created',
          subject: 'identity:id-1:self:created',
          payload: {},
          timestamp: new Date().toISOString(),
        },
      ],
      '/friends': [],
      '/friends/requests': [],
      '/presence': [],
    })

    const router = testRouter()
    router.push('/')
    await router.isReady()
    const wrapper = mount(Home, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('You created your identity.'))
    expect(wrapper.text()).toContain('View all')
  })
})
