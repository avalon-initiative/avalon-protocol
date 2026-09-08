// Confirms issue #130's actual point: the identity header is present
// regardless of which tab is active, and switching tabs doesn't lose the
// shell. Uses a real router (memory history) with the real nested-route
// config so a regression in the route nesting/meta-inheritance itself
// would fail this, not just a HubShell-in-isolation mount.
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createMemoryHistory } from 'vue-router'
import { mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import HubShell from './HubShell.vue'
import Profile from './Profile.vue'
import Friends from './Friends.vue'
import { useSessionStore } from '../stores/session'

function mockFetchOnce(body: unknown) {
  vi.stubGlobal(
    'fetch',
    vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: () => Promise.resolve(body),
      text: () => Promise.resolve(JSON.stringify(body)),
    }),
  )
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
})

describe('HubShell', () => {
  it('shows the identity header on the profile tab', async () => {
    useSessionStore().login('a-token')
    mockFetchOnce({
      identity_id: 'id-1',
      identity_created_at: 'now',
      display_name: 'Avalon Player',
      avatar_url: null,
    })

    const router = testRouter()
    router.push('/profile')
    await router.isReady()
    const wrapper = mount(HubShell, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Avalon Player'))
    expect(wrapper.text()).toContain('id-1')
    expect(wrapper.text()).toContain('Log out')
  })

  it('keeps the identity header when navigating to the friends tab', async () => {
    useSessionStore().login('a-token')
    mockFetchOnce({
      identity_id: 'id-1',
      identity_created_at: 'now',
      display_name: 'Avalon Player',
      avatar_url: null,
    })

    const router = testRouter()
    router.push('/friends')
    await router.isReady()
    const wrapper = mount(HubShell, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Avalon Player'))
    expect(wrapper.text()).toContain('Friends')
  })
})
