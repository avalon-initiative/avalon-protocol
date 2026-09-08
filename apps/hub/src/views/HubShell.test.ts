// The shell is present regardless of which page is active, and switching
// pages doesn't lose it. Uses a real router (memory history) with the real
// nested-route shape so a regression in the route nesting itself would
// fail this, not just a HubShell-in-isolation mount.
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createMemoryHistory } from 'vue-router'
import { mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import HubShell from './HubShell.vue'
import Profile from './Profile.vue'
import Friends from './Friends.vue'
import { useSessionStore } from '../stores/session'
import { FakeWebSocket, mockFetchByPath } from '../testing/fakes'

const profile = {
  identity_id: 'id-1',
  identity_created_at: 'now',
  display_name: 'Avalon Player',
  avatar_url: null,
  handle: 'Avalon Player#1234',
}

function testRouter() {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      {
        path: '/',
        component: HubShell,
        children: [
          { path: 'profile', name: 'profile', component: Profile },
          { path: 'friends', name: 'friends', component: Friends },
        ],
      },
    ],
  })
}

beforeEach(() => {
  localStorage.clear()
  setActivePinia(createPinia())
  vi.stubGlobal('WebSocket', FakeWebSocket)
  mockFetchByPath({
    '/me': profile,
    '/me/presence': { identity_id: 'id-1', status: 'Online', playing: null, updated_at: 'now' },
    '/me/devices': [],
    '/me/devices/grants': [],
    '/friends': [],
    '/friends/requests': [],
    '/presence': [],
  })
})

describe('HubShell', () => {
  it('shows the sidebar, user chip, and coming-soon entries on the profile page', async () => {
    useSessionStore().login('a-token')

    const router = testRouter()
    router.push('/profile')
    await router.isReady()
    const wrapper = mount(HubShell, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Avalon Player#1234'))
    expect(wrapper.text()).toContain('AVALON')
    expect(wrapper.text()).toContain('Home')
    expect(wrapper.text()).toContain('Soon')
    // Logout lives on the Profile page now, not in the shell header.
    expect(wrapper.text()).toContain('Log out')
  })

  it('keeps the shell when navigating to the friends page', async () => {
    useSessionStore().login('a-token')

    const router = testRouter()
    router.push('/friends')
    await router.isReady()
    const wrapper = mount(HubShell, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Avalon Player#1234'))
    expect(wrapper.text()).toContain('Add a friend')
    expect(wrapper.find('nav[aria-label="Primary"]').exists()).toBe(true)
  })

  it('publishes an Online heartbeat so the player reads as online to friends', async () => {
    useSessionStore().login('a-token')

    const router = testRouter()
    router.push('/profile')
    await router.isReady()
    const wrapper = mount(HubShell, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Online'))
    const calls = (fetch as ReturnType<typeof vi.fn>).mock.calls as [string, RequestInit][]
    const presenceCall = calls.find(([url, init]) => url.endsWith('/me/presence') && init.method === 'PUT')
    expect(presenceCall).toBeDefined()
  })
})
