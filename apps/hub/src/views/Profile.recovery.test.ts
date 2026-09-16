// Social recovery (issue #201) — the Profile page's guardian-management
// card and the "a recovery is in progress against you" notice. Same
// mounting approach Profile.test.ts already established.
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createMemoryHistory } from 'vue-router'
import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import Profile from './Profile.vue'
import { useSessionStore } from '../stores/session'
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

async function mountProfile(responses: Record<string, unknown>) {
  useSessionStore().login('a-token')
  mockFetchByPath({
    '/me': profile,
    '/me/passkeys': [{ id: 'p1', label: 'Laptop', added_at: 'now' }],
    '/friends': [],
    '/me/recovery/guardians': { guardian_ids: [], threshold: 0, updated_at: null },
    '/me/recovery/status': null,
    '/me/recovery/guardian-requests': [],
    '/me/recovery/guardian-of': [],
    ...responses,
  })

  const router = testRouter()
  router.push('/')
  await router.isReady()
  const wrapper = mount(Profile, { global: { plugins: [router] } })
  await vi.waitFor(() => expect(wrapper.text()).toContain('Recovery guardians'))
  return wrapper
}

describe('Recovery guardians card', () => {
  it('lists friends as guardian candidates', async () => {
    const wrapper = await mountProfile({
      '/friends': [{ a: 'id-1', b: 'friend-1', since: 'now' }],
      '/presence': [{ identity_id: 'friend-1', status: 'Online' }],
      '/identities/profiles': [
        { identity_id: 'friend-1', display_name: 'Bramble', avatar_url: null },
      ],
    })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Bramble'))
  })

  it('prompts to add a friend first when there are none yet', async () => {
    const wrapper = await mountProfile({})
    expect(wrapper.text()).toContain('You need at least one friend before you can designate a guardian.')
  })
})

describe('In-progress recovery notice', () => {
  it('shows a cancel prompt when a recovery is active against this identity', async () => {
    const wrapper = await mountProfile({
      '/me/recovery/status': {
        id: 'req-1',
        identity_id: 'id-1',
        status: 'delay',
        threshold: 2,
        approvals_count: 2,
        requested_at: 'now',
        delay_ends_at: 'later',
      },
    })
    await vi.waitFor(() =>
      expect(wrapper.text()).toContain('A recovery attempt is in progress against your identity'),
    )
    expect(wrapper.text()).toContain('This was not me — cancel it')
  })

  it('shows nothing extra when no recovery is in progress', async () => {
    const wrapper = await mountProfile({})
    expect(wrapper.text()).not.toContain('A recovery attempt is in progress')
  })
})

describe('Guardian-of card (issue #443)', () => {
  it('lists identities relying on this identity as a guardian, and can resign', async () => {
    const wrapper = await mountProfile({
      '/me/recovery/guardian-of': [
        {
          identity_id: 'owner-1',
          display_name: 'Bramble',
          added_at: 'now',
        },
      ],
    })
    await vi.waitFor(() => expect(wrapper.text()).toContain("You're a recovery guardian for"))
    expect(wrapper.text()).toContain('Bramble')

    const resignButton = wrapper
      .findAll('button')
      .find((b) => b.text() === 'Stop being a guardian')!
    await resignButton.trigger('click')
    await flushPromises()

    const resignCall = (fetch as ReturnType<typeof vi.fn>).mock.calls.find(([url, init]) =>
      String(url).includes('/me/recovery/guardian-of/owner-1') &&
      (init as RequestInit | undefined)?.method === 'DELETE',
    )
    expect(resignCall).toBeTruthy()
  })

  it('shows nothing when this identity is not a guardian for anyone', async () => {
    const wrapper = await mountProfile({})
    expect(wrapper.text()).not.toContain("You're a recovery guardian for")
  })
})
