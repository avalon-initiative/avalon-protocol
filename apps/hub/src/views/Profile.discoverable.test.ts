// Issue #205's opt-in global search toggle on the Profile page — a
// first-class control, not buried in a submenu, that flips
// `discoverable` via PATCH /me and reflects the server's own response
// value back (never assumes the click succeeded as sent).
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createMemoryHistory } from 'vue-router'
import { mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import Profile from './Profile.vue'
import { useSessionStore } from '../stores/session'

function testRouter() {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', name: 'profile', component: Profile },
      { path: '/login', name: 'login', component: Profile },
    ],
  })
}

const baseProfile = {
  identity_id: 'id-1',
  identity_created_at: 'now',
  display_name: 'Nova',
  avatar_url: null,
  handle: 'Nova#4821',
}

/**
 * Unlike `mockFetchByPath` (one static response per path, regardless of
 * method), this test needs `/me` to answer differently for the initial
 * `GET` versus the `PATCH` the toggle click sends — so it stubs `fetch`
 * directly rather than reusing that shared fixture.
 */
function mockMeSequence(discoverableSequence: boolean[]) {
  let call = 0
  vi.stubGlobal(
    'fetch',
    vi.fn().mockImplementation((url: string) => {
      const path = new URL(url, 'http://test').pathname
      let body: unknown
      if (path === '/me') {
        const discoverable = discoverableSequence[Math.min(call, discoverableSequence.length - 1)]
        call += 1
        body = { ...baseProfile, discoverable }
      } else if (path === '/me/passkeys') {
        body = []
      } else {
        body = undefined
      }
      return Promise.resolve({
        ok: true,
        status: 200,
        json: () => Promise.resolve(body),
        text: () => Promise.resolve(body === undefined ? '' : JSON.stringify(body)),
      })
    }),
  )
}

beforeEach(() => {
  localStorage.clear()
  setActivePinia(createPinia())
})

describe('Profile discoverability toggle (issue #205)', () => {
  it('shows "not publicly searchable" and a Turn on control by default', async () => {
    mockMeSequence([false])
    useSessionStore().login('a-token')

    const router = testRouter()
    router.push('/')
    await router.isReady()
    const wrapper = mount(Profile, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('You are not publicly searchable.'))
    expect(wrapper.text()).toContain('Turn on')
  })

  it('turning the toggle on reflects the server response, not an optimistic guess', async () => {
    mockMeSequence([false, true])
    useSessionStore().login('a-token')

    const router = testRouter()
    router.push('/')
    await router.isReady()
    const wrapper = mount(Profile, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Turn on'))

    const button = wrapper.findAll('button').find((b) => b.text().includes('Turn on'))
    expect(button).toBeTruthy()
    await button!.trigger('click')

    await vi.waitFor(() => expect(wrapper.text()).toContain('You are currently publicly searchable.'))
    expect(wrapper.text()).toContain('Turn off')
  })

  it('turning the toggle back off removes the "searchable" indicator immediately', async () => {
    mockMeSequence([true, false])
    useSessionStore().login('a-token')

    const router = testRouter()
    router.push('/')
    await router.isReady()
    const wrapper = mount(Profile, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('You are currently publicly searchable.'))

    const button = wrapper.findAll('button').find((b) => b.text().includes('Turn off'))
    expect(button).toBeTruthy()
    await button!.trigger('click')

    await vi.waitFor(() => expect(wrapper.text()).toContain('You are not publicly searchable.'))
  })
})
