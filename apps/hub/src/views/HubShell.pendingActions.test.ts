// Issue #466 — the Hub-wide pending-action badge aggregating incoming
// friend requests, guild join requests awaiting review, guild invites,
// device-grant approvals, guardian requests/designations, and unread DMs
// into a single count.
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createMemoryHistory } from 'vue-router'
import { mount, flushPromises } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import HubShell from './HubShell.vue'
import Profile from './Profile.vue'
import { useSessionStore } from '../stores/session'
import { FakeWebSocket, mockFetchByPath } from '../testing/fakes'

const profile = {
  identity_id: 'id-self',
  identity_created_at: 'now',
  display_name: 'Avalon User',
  avatar_url: null,
}

function testRouter() {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      {
        path: '/',
        component: HubShell,
        children: [{ path: 'profile', name: 'profile', component: Profile }],
      },
    ],
  })
}

function baseRoutes() {
  return {
    '/me': profile,
    '/me/presence': { identity_id: 'id-self', status: 'Online', playing: null, updated_at: 'now' },
    '/me/devices': [],
    '/me/devices/grants': [],
    '/friends': [],
    '/friends/requests': [],
    '/presence': [],
    '/me/guild-announcements': [],
    '/me/guilds': [],
    '/me/guild-invites': [],
    '/me/recovery/guardian-requests': [],
    '/me/recovery/guardian-of': [],
    '/conversations': [],
    '/ledger/sth/latest': {
      tree_size: 1,
      root_hash: 'ab'.repeat(32),
      network_id: 'avalon-test-fixture',
      signing_key_id: 'settlement-operator-1',
      signature: 'cd'.repeat(64),
      created_at: '2026-09-09T00:00:00Z',
    },
  }
}

beforeEach(() => {
  localStorage.clear()
  setActivePinia(createPinia())
  vi.stubGlobal('WebSocket', FakeWebSocket)
})

describe('HubShell pending-actions badge', () => {
  it('shows no badge and an empty panel when nothing is pending', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath(baseRoutes())

    const router = testRouter()
    router.push('/profile')
    await router.isReady()
    const wrapper = mount(HubShell, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Avalon User'))

    const bell = wrapper.find('[aria-label="Pending actions"]')
    expect(bell.exists()).toBe(true)
    await bell.trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('Nothing waiting on you.')
  })

  it('sums every source into one combined count', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath({
      ...baseRoutes(),
      '/friends/requests': [
        { id: 'r1', from: 'id-other', to: 'id-self', requested_at: 'now' },
        // An outgoing request the caller sent themselves never counts.
        { id: 'r2', from: 'id-self', to: 'id-other-2', requested_at: 'now' },
      ],
      '/me/guilds': [{ guild_id: 'g1', role_index: 0, joined_at: 'now' }],
      '/guilds/g1': { id: 'g1', name: 'Dragon Hunters' },
      '/guilds/g1/join-requests': [
        { id: 'jr1', guild_id: 'g1', identity_id: 'id-applicant', requested_at: 'now' },
      ],
      '/me/guild-invites': [{ id: 'inv1', guild_id: 'g1', from: 'id-owner', to: 'id-self', status: 'pending' }],
      '/me/devices/grants': [{ id: 'grant1', status: 'pending', created_at: 'now' }],
      '/me/recovery/guardian-requests': [
        { request: { id: 'gr1', identity_id: 'id-recovering', status: 'pending' } },
      ],
      '/me/recovery/guardian-of': [
        { identity_id: 'id-ward', display_name: 'Ward', added_at: 'now' },
      ],
      '/conversations': [{ id: 'convo1', participants: ['id-self', 'id-other'] }],
      '/conversations/convo1/messages': [
        { id: 'm1', conversation_id: 'convo1', author: 'id-other', body: 'hi', sent_at: 'now' },
      ],
    })

    const router = testRouter()
    router.push('/profile')
    await router.isReady()
    const wrapper = mount(HubShell, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Avalon User'))

    // 1 friend request + 1 join request + 1 invite + 1 device grant +
    // 1 guardian request + 1 new guardian-of + 1 unread DM = 7.
    await vi.waitFor(() => expect(wrapper.find('[aria-label*="7 waiting"]').exists()).toBe(true))

    const bell = wrapper.find('[aria-label*="Pending actions"]')
    await bell.trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('Friend requests')
    expect(wrapper.text()).toContain('Guild join requests')
    expect(wrapper.text()).toContain('Guild invites')
    expect(wrapper.text()).toContain('Device approval requests')
    expect(wrapper.text()).toContain('Recovery requests to approve')
    expect(wrapper.text()).toContain('New guardian designations')
    expect(wrapper.text()).toContain('Unread messages')
  })

  // Opening a conversation (useConversationThread's own markConversationSeen
  // call, exercised directly here rather than via a full Messages.vue
  // mount, which would just re-cover that composable's already-tested load
  // path) is what clears its contribution — the next poll tick picks up
  // the new localStorage state, no remount needed.
  it('drops a conversation from the unread count once its thread is opened', async () => {
    useSessionStore().login('a-token')
    vi.useFakeTimers()
    mockFetchByPath({
      ...baseRoutes(),
      '/conversations': [{ id: 'convo1', participants: ['id-self', 'id-other'] }],
      '/conversations/convo1/messages': [
        { id: 'm1', conversation_id: 'convo1', author: 'id-other', body: 'hi', sent_at: 'now' },
      ],
    })

    const router = testRouter()
    router.push('/profile')
    await router.isReady()
    const wrapper = mount(HubShell, { global: { plugins: [router] } })
    await flushPromises()
    expect(wrapper.find('[aria-label*="1 waiting"]').exists()).toBe(true)

    const { markConversationSeen } = await import('../api/notifications')
    markConversationSeen('convo1', 'now')

    // Advance past useNotificationSummary's own poll interval.
    await vi.advanceTimersByTimeAsync(30_000)
    await flushPromises()
    expect(wrapper.find('[aria-label="Pending actions"]').exists()).toBe(true)
    expect(wrapper.find('[aria-label*="waiting"]').exists()).toBe(false)

    vi.useRealTimers()
  })
})
