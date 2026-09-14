// The shell is present regardless of which page is active, and switching
// pages doesn't lose it. Uses a real router (memory history) with the real
// nested-route shape so a regression in the route nesting itself would
// fail this, not just a HubShell-in-isolation mount.
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createMemoryHistory } from 'vue-router'
import { mount, flushPromises } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import HubShell from './HubShell.vue'
import Profile from './Profile.vue'
import Friends from './Friends.vue'
import { useSessionStore } from '../stores/session'
import { FakeWebSocket, mockFetchByPath } from '../testing/fakes'

const profile = {
  identity_id: 'id-1',
  identity_created_at: 'now',
  display_name: 'Avalon User',
  avatar_url: null,
  handle: 'Avalon User#1234',
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
    '/me/guild-announcements': [],
    '/ledger/sth/latest': {
      tree_size: 1,
      root_hash: 'ab'.repeat(32),
      network_id: 'avalon-test-fixture',
      signing_key_id: 'settlement-operator-1',
      signature: 'cd'.repeat(64),
      created_at: '2026-09-09T00:00:00Z',
    },
  })
})

describe('HubShell', () => {
  it('shows the sidebar, user chip, and coming-soon entries on the profile page', async () => {
    useSessionStore().login('a-token')

    const router = testRouter()
    router.push('/profile')
    await router.isReady()
    const wrapper = mount(HubShell, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Avalon User#1234'))
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
    await vi.waitFor(() => expect(wrapper.text()).toContain('Avalon User#1234'))
    expect(wrapper.text()).toContain('Add a friend')
    expect(wrapper.find('nav[aria-label="Primary"]').exists()).toBe(true)
  })

  it('publishes an Online heartbeat so the user reads as online to friends', async () => {
    useSessionStore().login('a-token')

    const router = testRouter()
    router.push('/profile')
    await router.isReady()
    mount(HubShell, { global: { plugins: [router] } })
    // Waiting on the page containing "Online" is ambiguous — NetworkStatus
    // renders its own "Online"/"Offline" connectivity text regardless of
    // presence — so wait on the actual heartbeat call instead.
    await vi.waitFor(() => {
      const calls = (fetch as ReturnType<typeof vi.fn>).mock.calls as [string, RequestInit][]
      const presenceCall = calls.find(([url, init]) => url.endsWith('/me/presence') && init.method === 'PUT')
      expect(presenceCall).toBeDefined()
    })
  })

  // Issue #390: manually picking a status must actually publish it, and
  // must not get silently overwritten back to Online by the next heartbeat
  // tick (the server's `PUT /me/presence` treats Away/DoNotDisturb/Offline
  // as sticky overrides — the client has to honor that, not keep insisting
  // on Online).
  it('lets the user manually set their status and does not overwrite it on the next heartbeat', async () => {
    useSessionStore().login('a-token')
    vi.useFakeTimers()

    const router = testRouter()
    router.push('/profile')
    await router.isReady()
    const wrapper = mount(HubShell, { global: { plugins: [router] } })
    await flushPromises()

    mockFetchByPath({
      '/me/presence': { identity_id: 'id-1', status: 'DoNotDisturb', playing: null, updated_at: 'now' },
    })

    const select = wrapper.find('select[aria-label="Set your status"]')
    await select.setValue('DoNotDisturb')
    await flushPromises()
    expect(wrapper.text()).toContain('Do Not Disturb')

    // Advancing past a heartbeat tick must not silently flip the badge back
    // to Online — asserting on the rendered badge (rather than the raw
    // fetch call log) since fetch is a single global spy shared across
    // whatever else is mounted in this test file.
    await vi.advanceTimersByTimeAsync(60_000)
    await flushPromises()
    expect(wrapper.text()).toContain('Do Not Disturb')

    vi.useRealTimers()
  })
})
