// Issue #460's "Blocked users" card on Profile.vue: lists the caller's
// own outgoing blocks (never who has blocked the caller — see
// crates/server/src/blocks.rs's module doc comment) with an unblock
// action, plus a block-by-id/handle form for blocking someone directly.
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createMemoryHistory } from 'vue-router'
import { mount, flushPromises } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import Profile from './Profile.vue'
import { useSessionStore } from '../stores/session'
import { mockFetchByPath } from '../testing/fakes'

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
    routes: [{ path: '/', component: Profile }],
  })
}

beforeEach(() => {
  localStorage.clear()
  setActivePinia(createPinia())
})

describe('Profile blocked users', () => {
  it('shows an empty state when nothing is blocked', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath({ '/me': profile, '/me/passkeys': [], '/blocks': [] })

    const router = testRouter()
    router.push('/')
    await router.isReady()
    const wrapper = mount(Profile, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Blocked users'))

    expect(wrapper.text()).toContain("You haven't blocked anyone.")
  })

  it('lists a blocked user with their resolved display name and unblocks them', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath({
      '/me': profile,
      '/me/passkeys': [],
      '/blocks': [{ blocked: 'id-2', created_at: 'now' }],
      '/identities/profiles': [
        { identity_id: 'id-2', display_name: 'Grief', discriminator: '9001', avatar_url: null },
      ],
    })

    const router = testRouter()
    router.push('/')
    await router.isReady()
    const wrapper = mount(Profile, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Grief'))

    const unblockButton = wrapper.findAll('button').find((b) => b.text() === 'Unblock')!
    await unblockButton.trigger('click')
    await flushPromises()

    const unblockCall = (fetch as ReturnType<typeof vi.fn>).mock.calls.find(([url, init]) => {
      const method = (init as RequestInit | undefined)?.method
      return String(url).endsWith('/blocks/id-2') && method === 'DELETE'
    })
    expect(unblockCall).toBeTruthy()
  })

  it('blocks by identity id typed directly into the form', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath({ '/me': profile, '/me/passkeys': [], '/blocks': [] })

    const router = testRouter()
    router.push('/')
    await router.isReady()
    const wrapper = mount(Profile, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Blocked users'))

    await wrapper.find('input[placeholder="identity id or handle#1234"]').setValue('id-3')
    const blockButton = wrapper.findAll('button').find((b) => b.text() === 'Block')!
    await blockButton.trigger('click')
    await flushPromises()

    const blockCall = (fetch as ReturnType<typeof vi.fn>).mock.calls.find(([url, init]) => {
      const method = (init as RequestInit | undefined)?.method
      return String(url).endsWith('/blocks') && method === 'POST'
    })
    expect(blockCall).toBeTruthy()
    const body = JSON.parse((blockCall![1] as RequestInit).body as string)
    expect(body.identity_id).toBe('id-3')
  })
})
