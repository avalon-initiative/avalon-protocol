// #199: the single-passkey total-loss warning on the Passkeys card — shows
// while GET /me/passkeys returns exactly one passkey, hides once a second
// is registered. shouldShowSinglePasskeyWarning's own unit tests
// (../utils/singlePasskeyWarning.test.ts) cover the boundary logic in
// isolation; this covers it wired into the real page.
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createMemoryHistory } from 'vue-router'
import { mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import Profile from './Profile.vue'
import { useSessionStore } from '../api/session'
import { mockFetchByPath } from '../testing/fakes'

const profile = {
  identity_id: 'id-1',
  identity_created_at: 'now',
  display_name: 'Nova',
  avatar_url: null,
}

function testRouter() {
  return createRouter({
    history: createMemoryHistory(),
    routes: [{ path: '/', component: Profile }],
  })
}

beforeEach(() => {
  localStorage.clear()
  setActivePinia(createPinia())
})

// Seeds a bearer token and drives the real session-store initialize() path
// (a GET /me round trip, same as production) rather than constructing an
// AccountSession by hand — `mockFetchByPath` must already be stubbed
// before this runs.
async function loginTestSession() {
  localStorage.setItem('avalon:session:token', 'a-token')
  await useSessionStore().initialize()
}

async function mountProfile(passkeys: unknown[]) {
  mockFetchByPath({ '/me': profile, '/me/passkeys': passkeys })
  await loginTestSession()

  const router = testRouter()
  router.push('/')
  await router.isReady()
  const wrapper = mount(Profile, { global: { plugins: [router] } })
  await vi.waitFor(() => expect(wrapper.text()).toContain('Passkeys'))
  return wrapper
}

describe('Profile single-passkey warning', () => {
  it('shows the total-loss warning when there is exactly one passkey', async () => {
    const wrapper = await mountProfile([{ id: 'p1', label: null, added_at: 'now' }])
    await vi.waitFor(() => expect(wrapper.text()).toContain('You have only one passkey'))
  })

  it('hides the warning once a second passkey exists', async () => {
    const wrapper = await mountProfile([
      { id: 'p1', label: null, added_at: 'now' },
      { id: 'p2', label: 'Laptop', added_at: 'now' },
    ])
    await vi.waitFor(() => expect(wrapper.text()).toContain('Laptop'))
    expect(wrapper.text()).not.toContain('You have only one passkey')
  })

  // A safety-critical, non-dismissible warning must never silently
  // disappear just because a response didn't look like the expected array
  // — a malformed `GET /me/passkeys` body (still a 200, just an unexpected
  // shape) must surface as an honest error rather than being quietly
  // treated as "0 passkeys, nothing to warn about."
  it('surfaces an error instead of silently treating a malformed passkey list as empty', async () => {
    mockFetchByPath({ '/me': profile, '/me/passkeys': null })
    await loginTestSession()

    const router = testRouter()
    router.push('/')
    await router.isReady()
    const wrapper = mount(Profile, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Passkeys'))

    await vi.waitFor(() =>
      expect(wrapper.text()).toContain('Unexpected response fetching passkeys.'),
    )
  })
})
