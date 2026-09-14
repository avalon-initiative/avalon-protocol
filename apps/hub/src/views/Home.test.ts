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
      { path: '/connections', component: Home },
      { path: '/integrations', name: 'integrations', component: Home },
      { path: '/guilds', name: 'guilds', component: Home },
      { path: '/guilds/:id', name: 'guild', component: Home },
      { path: '/guilds/:id/channels/:cid', name: 'guild-channel', component: Home },
      { path: '/integrations/:slug', name: 'integration-profile', component: Home },
    ],
  })
}

const BASE_MOCKS = {
  '/me/guilds': [],
  '/me/connections': [],
}

beforeEach(() => {
  localStorage.clear()
  setActivePinia(createPinia())
  vi.stubGlobal('WebSocket', FakeWebSocket)
})

describe('Home', () => {
  it('welcomes the user by display name', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath({
      '/me': profile,
      '/me/history': [],
      '/friends': [],
      '/friends/requests': [],
      '/presence': [],
      ...BASE_MOCKS,
    })

    const router = testRouter()
    router.push('/')
    await router.isReady()
    const wrapper = mount(Home, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Welcome back, Nova'))
  })

  it('renders sensible empty states with no friends and no history', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath({
      '/me': profile,
      '/me/history': [],
      '/friends': [],
      '/friends/requests': [],
      '/presence': [],
      ...BASE_MOCKS,
    })

    const router = testRouter()
    router.push('/')
    await router.isReady()
    const wrapper = mount(Home, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('No activity yet'))
    expect(wrapper.text()).toContain('No friends online right now.')
    expect(wrapper.text()).toContain('Quick Actions')
    expect(wrapper.text()).toContain("You haven't connected anything yet.")
    expect(wrapper.text()).toContain("You haven't joined a guild yet.")
    expect(wrapper.text()).toContain('No guild messages yet.')
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
      ...BASE_MOCKS,
    })

    const router = testRouter()
    router.push('/')
    await router.isReady()
    const wrapper = mount(Home, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('You created your identity.'))
    expect(wrapper.text()).toContain('View all')
  })

  it('shows connected integrators, guilds, and their latest messages', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath({
      '/me': profile,
      '/me/history': [],
      '/friends': [],
      '/friends/requests': [],
      '/presence': [],
      '/me/connections': [
        {
          binding_id: 'bind-1',
          integrator_id: 'integrator-1',
          slug: 'echoes-of-aether',
          name: 'Echoes of Aether',
          established_at: '2026-09-01T00:00:00Z',
          grants: [],
        },
      ],
      '/me/guilds': [{ guild_id: 'guild-1', role_index: 0, joined_at: '2026-09-01T00:00:00Z' }],
      '/guilds/guild-1': {
        id: 'guild-1',
        name: 'Celestial Forge',
        tag: 'FORGE',
        description: '',
        owner: 'id-1',
        created_at: '2026-09-01T00:00:00Z',
        member_count: 42,
        integrators: [],
        join_policy: 'invite_only',
        motd: null,
        banner: null,
        icon: null,
        links: [],
        recruiting: false,
        game_breakdown_public: false,
        favorite_games: [],
      },
      '/guilds/guild-1/channels': [
        { id: 'chan-1', guild_id: 'guild-1', name: 'general', archived: false, created_at: '2026-09-01T00:00:00Z', announcement_only: false },
      ],
      '/guilds/guild-1/channels/chan-1/messages': [
        { id: 'msg-1', channel_id: 'chan-1', author: 'id-2', body: "Let's run the dungeon tonight!", sent_at: new Date().toISOString() },
      ],
    })

    const router = testRouter()
    router.push('/')
    await router.isReady()
    const wrapper = mount(Home, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Echoes of Aether'))
    expect(wrapper.text()).toContain('Celestial Forge')
    expect(wrapper.text()).toContain('FORGE · 42 members')
    await vi.waitFor(() => expect(wrapper.text()).toContain("Let's run the dungeon tonight!"))
    expect(wrapper.text()).toContain('Recently Connected')
  })
})
