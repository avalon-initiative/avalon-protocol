// Issue #280: the bell in the shell header surfaces recent guild
// announcement-only channel posts, with an unread badge and a
// click-to-open panel — see api/guildAnnouncements.ts for the
// read/unread logic this view drives.
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createMemoryHistory } from 'vue-router'
import { mount, flushPromises, type VueWrapper } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { h } from 'vue'
import HubShell from './HubShell.vue'
import Profile from './Profile.vue'
import { useSessionStore } from '../stores/session'
import { FakeWebSocket, mockFetchByPath } from '../testing/fakes'

// A trivial stand-in for the real `Guild.vue` at the target route — this
// suite is only exercising HubShell's own state after selecting an alert,
// not Guild.vue's own (heavily-mocked-data-dependent) behavior.
const GuildStub = { render: () => h('div', 'guild view') }

const profile = {
  identity_id: 'id-1',
  identity_created_at: 'now',
  display_name: 'Avalon User',
  avatar_url: null,
  handle: 'Avalon User#1234',
}

const announcement = {
  message_id: 'msg-1',
  channel_id: 'chan-1',
  channel_name: 'announcements',
  guild_id: 'guild-1',
  author: 'id-2',
  body: 'server maintenance tonight',
  sent_at: '2026-09-14T12:00:00Z',
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
          { path: 'guilds/:id', name: 'guild', component: GuildStub },
          { path: 'guilds/:id/channels/:cid', name: 'guild-channel', component: GuildStub },
        ],
      },
    ],
  })
}

let mountedWrappers: VueWrapper[] = []

beforeEach(() => {
  localStorage.clear()
  setActivePinia(createPinia())
  vi.stubGlobal('WebSocket', FakeWebSocket)
  mountedWrappers = []
})

afterEach(() => {
  // Unmounting matters here beyond hygiene: HubShell's own onUnmounted
  // clears its presence-heartbeat and announcements-poll intervals — an
  // un-unmounted wrapper from an earlier test otherwise keeps polling and
  // patching a still-attached (real Vue Test Utils default: attached to
  // document.body) DOM tree, which a later test's own `findAll` for the
  // same selector then also matches.
  mountedWrappers.forEach((wrapper) => wrapper.unmount())
})

async function mountAtProfile(announcements: unknown[]) {
  mockFetchByPath({
    '/me': profile,
    '/me/presence': { identity_id: 'id-1', status: 'Online', playing: null, updated_at: 'now' },
    '/me/guild-announcements': announcements,
    '/ledger/sth/latest': {
      tree_size: 1,
      root_hash: 'ab'.repeat(32),
      network_id: 'avalon-test-fixture',
      signing_key_id: 'settlement-operator-1',
      signature: 'cd'.repeat(64),
      created_at: '2026-09-09T00:00:00Z',
    },
  })

  useSessionStore().login('a-token')
  const router = testRouter()
  router.push('/profile')
  await router.isReady()
  const wrapper = mount(HubShell, { global: { plugins: [router] } })
  mountedWrappers.push(wrapper)
  await vi.waitFor(() => expect(wrapper.text()).toContain('Avalon User#1234'))
  await flushPromises()
  return { wrapper, router }
}

describe('HubShell guild announcements', () => {
  it('shows an unread badge when there are unseen announcements', async () => {
    const { wrapper } = await mountAtProfile([announcement])
    const bell = wrapper.find('button[aria-label*="unread"]')
    expect(bell.exists()).toBe(true)
    expect(bell.text()).toContain('1')
  })

  it('shows no badge when there are no announcements', async () => {
    const { wrapper } = await mountAtProfile([])
    expect(wrapper.find('button[aria-label*="unread"]').exists()).toBe(false)
  })

  it('opens the panel on click and lists the announcement', async () => {
    const { wrapper } = await mountAtProfile([announcement])
    await wrapper.find('button[aria-label*="Guild announcements"]').trigger('click')
    expect(wrapper.text()).toContain('#announcements')
    expect(wrapper.text()).toContain('server maintenance tonight')
  })

  it('navigates to the channel and marks it seen (localStorage) when an alert is clicked', async () => {
    const { wrapper, router } = await mountAtProfile([announcement])
    await wrapper.find('button[aria-label*="Guild announcements"]').trigger('click')

    const item = wrapper.findAll('button').find((b) => b.text().includes('server maintenance tonight'))!
    await item.trigger('click')
    await flushPromises()

    expect(router.currentRoute.value.name).toBe('guild-channel')
    expect(router.currentRoute.value.params).toEqual({ id: 'guild-1', cid: 'chan-1' })
    expect(localStorage.getItem('avalon:guildAnnouncements:lastSeen')).toContain(announcement.sent_at)
  })

  it('persists read state across a remount (localStorage)', async () => {
    const { wrapper } = await mountAtProfile([announcement])
    await wrapper.find('button[aria-label*="Guild announcements"]').trigger('click')
    const item = wrapper.findAll('button').find((b) => b.text().includes('server maintenance tonight'))!
    await item.trigger('click')
    await flushPromises()

    // A fresh mount (simulating a page reload) must still treat the same
    // channel/timestamp as already seen — the whole point of persisting to
    // localStorage rather than a component-local ref.
    const { wrapper: reloaded } = await mountAtProfile([announcement])
    expect(reloaded.find('button[aria-label*="unread"]').exists()).toBe(false)
  })
})
