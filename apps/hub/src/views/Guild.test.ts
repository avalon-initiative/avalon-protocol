// Tab switching and the Channels tab's persistent sidebar (issue #241):
// Guild.vue used to be one long scrolling page, with guild chat entirely on
// its own route (GuildChannel.vue). This covers the new tabbed layout, that
// picking a different channel in the Channels tab updates in place (no
// route navigation / component remount, useGuildChat just reloads), and
// that `/guilds/:id/channels/:cid` still deep-links straight into the
// Channels tab with that channel pre-selected.
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createMemoryHistory } from 'vue-router'
import { mount, flushPromises } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import Guild from './Guild.vue'
import { useSessionStore } from '../stores/session'
import { FakeWebSocket, mockFetchByPath } from '../testing/fakes'

const profile = {
  identity_id: 'id-owner',
  identity_created_at: 'now',
  display_name: 'Nova',
  avatar_url: null,
  handle: 'Nova#4821',
}

const guild = {
  id: 'g1',
  name: 'Dragon Hunters',
  tag: 'DRGN',
  description: 'A guild.',
  owner: 'id-owner',
  created_at: 'now',
  member_count: 1,
  games: [],
  join_policy: 'invite_only',
  motd: null,
  banner: null,
  links: [],
  recruiting: false,
  game_breakdown_public: false,
  favorite_games: [],
}

const roles = [{ name_index: 0, name: 'owner', permissions: ['manage_guild', 'manage_roles', 'manage_members', 'manage_channels'] }]

const members = [{ guild_id: 'g1', identity_id: 'id-owner', role_index: 0, joined_at: 'now' }]

const channels = [
  { id: 'c1', name: 'general', archived: false },
  { id: 'c2', name: 'raids', archived: false },
]

function messagesFor(channelId: string) {
  return [
    {
      id: `m-${channelId}-1`,
      channel_id: channelId,
      author: 'id-owner',
      body: `Hello from ${channelId}`,
      sent_at: 'now',
    },
  ]
}

function baseRoutes() {
  return {
    '/me': profile,
    '/guilds/g1': guild,
    '/guilds/g1/roles': roles,
    '/guilds/g1/members': members,
    '/presence': [],
    '/identities/profiles': [],
    '/guilds/g1/channels': channels,
    '/guilds/g1/events': [],
    // The owner (canManageGuild) can always fetch the breakdown regardless
    // of the public-exposure toggle.
    '/guilds/g1/game-breakdown': { guild_id: 'g1', total_members: 1, breakdown: [] },
    '/guilds/g1/channels/c1/messages': messagesFor('c1'),
    '/guilds/g1/channels/c2/messages': messagesFor('c2'),
  }
}

function testRouter() {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/guilds/:id', name: 'guild', component: Guild },
      { path: '/guilds/:id/channels/:cid', name: 'guild-channel', component: Guild },
      { path: '/guilds', name: 'guilds', component: Guild },
    ],
  })
}

beforeEach(() => {
  localStorage.clear()
  setActivePinia(createPinia())
  vi.stubGlobal('WebSocket', FakeWebSocket)
})

describe('Guild', () => {
  it('renders Overview by default and switches tabs on click', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath(baseRoutes())

    const router = testRouter()
    router.push('/guilds/g1')
    await router.isReady()
    const wrapper = mount(Guild, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Dragon Hunters'))

    // Overview content (game history card is always present).
    expect(wrapper.text()).toContain('History')
    // Members-only content isn't rendered yet.
    expect(wrapper.text()).not.toContain('Search by identity id')

    const tabs = wrapper.findAll('button').filter((b) => ['Members', 'Channels', 'Events', 'Roles', 'Settings'].includes(b.text()))
    const membersTab = tabs.find((b) => b.text() === 'Members')!
    await membersTab.trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('Currently playing')
  })

  it('shows a channel sidebar and switches channels without a route navigation', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath(baseRoutes())

    const router = testRouter()
    router.push('/guilds/g1')
    await router.isReady()
    const wrapper = mount(Guild, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Dragon Hunters'))

    const channelsTab = wrapper.findAll('button').find((b) => b.text() === 'Channels')!
    await channelsTab.trigger('click')
    await flushPromises()
    await vi.waitFor(() => expect(wrapper.text()).toContain('Hello from c1'))

    // Both channels are listed in the persistent sidebar at once.
    expect(wrapper.text()).toContain('general')
    expect(wrapper.text()).toContain('raids')
    expect(router.currentRoute.value.name).toBe('guild')

    const raidsButton = wrapper.findAll('button').find((b) => b.text().includes('raids'))!
    await raidsButton.trigger('click')
    await flushPromises()
    await vi.waitFor(() => expect(wrapper.text()).toContain('Hello from c2'))

    // Selecting a channel updates the URL (deep-linkable) without a full
    // navigation — the same Guild component instance renders both states.
    expect(router.currentRoute.value.name).toBe('guild-channel')
    expect(router.currentRoute.value.params.cid).toBe('c2')
  })

  it('deep-links /guilds/:id/channels/:cid straight into the Channels tab', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath(baseRoutes())

    const router = testRouter()
    router.push('/guilds/g1/channels/c2')
    await router.isReady()
    const wrapper = mount(Guild, { global: { plugins: [router] } })

    await vi.waitFor(() => expect(wrapper.text()).toContain('Hello from c2'))
    expect(wrapper.text()).toContain('raids')
  })
})
