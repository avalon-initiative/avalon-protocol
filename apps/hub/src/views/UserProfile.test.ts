// Issue #393: clicking a friend or guild member opens their read-only
// profile card. Covers both the click-through from Friends.vue and the
// card itself.
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createMemoryHistory } from 'vue-router'
import { mount, flushPromises } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import Friends from './Friends.vue'
import UserProfile from './UserProfile.vue'
import { useSessionStore } from '../stores/session'
import { FakeWebSocket, mockFetchByPath } from '../testing/fakes'

const selfProfile = {
  identity_id: 'id-self',
  identity_created_at: 'now',
  display_name: 'Nova',
  avatar_url: null,
  handle: 'Nova#4821',
  discoverable: false,
}

function testRouter() {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/friends', name: 'friends', component: Friends },
      { path: '/users/:id', name: 'user-profile', component: UserProfile },
    ],
  })
}

beforeEach(() => {
  localStorage.clear()
  setActivePinia(createPinia())
  vi.stubGlobal('WebSocket', FakeWebSocket)
})

describe('viewing another user from Friends', () => {
  it('navigates to the profile card when a friend row is clicked', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath({
      '/me': selfProfile,
      '/friends': [{ a: 'id-self', b: 'id-friend', since: 'now' }],
      '/friends/requests': [],
      '/presence': [{ identity_id: 'id-friend', status: 'Online', playing: null, updated_at: 'now' }],
      '/identities/profiles': [
        { identity_id: 'id-friend', display_name: 'Ilya', discriminator: '1122', avatar_url: null },
      ],
      '/people/discover': { candidates: [] },
    })

    const router = testRouter()
    router.push('/friends')
    await router.isReady()
    const wrapper = mount(Friends, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Ilya'))

    const nameButton = wrapper.findAll('button').find((b) => b.text().includes('Ilya'))!
    await nameButton.trigger('click')
    await flushPromises()

    expect(router.currentRoute.value.name).toBe('user-profile')
    expect(router.currentRoute.value.params.id).toBe('id-friend')
  })
})

describe('UserProfile', () => {
  function testRouterForCard() {
    return createRouter({
      history: createMemoryHistory(),
      routes: [{ path: '/users/:id', name: 'user-profile', component: UserProfile }],
    })
  }

  it("shows the user's public profile fields and live presence", async () => {
    useSessionStore().login('a-token')
    mockFetchByPath({
      '/identities/profiles': [
        { identity_id: 'id-friend', display_name: 'Ilya', discriminator: '1122', avatar_url: null },
      ],
      '/presence': [{ identity_id: 'id-friend', status: 'DoNotDisturb', playing: null, updated_at: 'now' }],
    })

    const router = testRouterForCard()
    router.push('/users/id-friend')
    await router.isReady()
    const wrapper = mount(UserProfile, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Ilya'))

    expect(wrapper.text()).toContain('Ilya#1122')
    expect(wrapper.text()).toContain('Do Not Disturb')
    // #403 (open decision): bio/self-description fields are deliberately
    // not requested/shown here yet.
  })

  it("shows a not-found message when the user doesn't resolve", async () => {
    useSessionStore().login('a-token')
    mockFetchByPath({
      '/identities/profiles': [],
      '/presence': [],
    })

    const router = testRouterForCard()
    router.push('/users/id-missing')
    await router.isReady()
    const wrapper = mount(UserProfile, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain("couldn't be found"))
  })
})
