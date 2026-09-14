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
  icon: null,
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
    '/guilds/g1/join-requests': [],
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
      { path: '/users/:id', name: 'user-profile', component: Guild },
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

  it('shows the icon badge in the header when set, and nothing when unset', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath(baseRoutes())

    const router = testRouter()
    router.push('/guilds/g1')
    await router.isReady()
    const wrapper = mount(Guild, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Dragon Hunters'))

    // guild.icon is null in this fixture — no badge image rendered.
    expect(wrapper.find('header img').exists()).toBe(false)
  })

  it('shows the icon badge in the header when guild.icon is set', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath({
      ...baseRoutes(),
      '/guilds/g1': { ...guild, icon: 'https://example.com/icon.png' },
    })

    const router = testRouter()
    router.push('/guilds/g1')
    await router.isReady()
    const wrapper = mount(Guild, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Dragon Hunters'))

    const headerIcon = wrapper.find('header img')
    expect(headerIcon.exists()).toBe(true)
    expect(headerIcon.attributes('src')).toBe('https://example.com/icon.png')
  })

  // Per-member RSVP roster (issue #248): clicking an event card on the
  // Events tab opens the shared roster panel, which fetches the raw
  // guild_event_rsvps rows and resolves display names via the batched
  // profiles endpoint.
  it('opens the RSVP roster panel with resolved names when an event card is clicked', async () => {
    useSessionStore().login('a-token')
    const event = {
      id: 'ev1',
      guild_id: 'g1',
      channel_id: null,
      title: 'Raid night',
      description: null,
      starts_at: '2026-09-15T20:00:00Z',
      ends_at: null,
      created_by: 'id-owner',
      created_at: '2026-09-01T00:00:00Z',
      rsvp_counts: { going: 1, maybe: 0, not_going: 0 },
    }
    mockFetchByPath({
      ...baseRoutes(),
      '/guilds/g1/events': [event],
      '/guilds/g1/events/ev1/rsvps': [
        { identity_id: 'id-owner', status: 'going', responded_at: '2026-09-02T00:00:00Z' },
      ],
      '/identities/profiles': [
        { identity_id: 'id-owner', display_name: 'Nova', discriminator: '4821' },
      ],
    })

    const router = testRouter()
    router.push('/guilds/g1')
    await router.isReady()
    const wrapper = mount(Guild, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Dragon Hunters'))

    const eventsTab = wrapper.findAll('button').find((b) => b.text() === 'Events')!
    await eventsTab.trigger('click')
    await flushPromises()
    await vi.waitFor(() => expect(wrapper.text()).toContain('Raid night'))

    const card = wrapper.find('[class*="eventCardClickable"]')
    expect(card.exists()).toBe(true)
    await card.trigger('click')
    await flushPromises()
    await vi.waitFor(() => expect(wrapper.find('[role="dialog"]').exists()).toBe(true))

    expect(wrapper.text()).toContain('Going (1)')
    expect(wrapper.text()).toContain('Nova#4821')
  })

  // The owner role's permissions are structural (guild.owner, not this
  // row) — attempting to uncheck one in the matrix must be rejected
  // without even reaching the server, and the checkbox (which the browser
  // already flipped natively before this handler ran) must snap back to
  // checked rather than being left showing a state that was never applied.
  it("rejects unchecking an owner permission and reverts the checkbox", async () => {
    useSessionStore().login('a-token')
    mockFetchByPath(baseRoutes())

    const router = testRouter()
    router.push('/guilds/g1')
    await router.isReady()
    const wrapper = mount(Guild, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Dragon Hunters'))

    const rolesTab = wrapper.findAll('button').find((b) => b.text() === 'Roles')!
    await rolesTab.trigger('click')
    await flushPromises()

    const unlockButton = wrapper.find('[aria-label="Unlock role to edit"]')
    expect(unlockButton.exists()).toBe(true)
    await unlockButton.trigger('click')
    await flushPromises()

    const checkbox = wrapper.find('input[type="checkbox"]')
    expect((checkbox.element as HTMLInputElement).checked).toBe(true)

    ;(checkbox.element as HTMLInputElement).checked = false
    await checkbox.trigger('change')
    await flushPromises()

    expect((checkbox.element as HTMLInputElement).checked).toBe(true)
    expect(wrapper.text()).toContain("always has every permission")
  })

  // Issue #392: the invite field used to send whatever was typed straight
  // through as a raw UUID, opaque-422ing on anything else. It now accepts
  // a display_name#1234 handle (resolved first, same as Friends.vue's
  // add-friend flow) and rejects anything that isn't an id or handle
  // client-side, with a clear message, before ever hitting the network.
  it('invites by identity id, resolves a handle first, and rejects neither', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath({
      ...baseRoutes(),
      '/guilds/g1/invites': { id: 'inv1', guild_id: 'g1', to: 'id-outsider', status: 'pending' },
      '/friends/handle/Nova%234821': { identity_id: '11111111-2222-3333-4444-555555555555' },
    })

    const router = testRouter()
    router.push('/guilds/g1')
    await router.isReady()
    const wrapper = mount(Guild, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Dragon Hunters'))

    const membersTab = wrapper.findAll('button').find((b) => b.text() === 'Members')!
    await membersTab.trigger('click')
    await flushPromises()

    const openInviteButton = wrapper.findAll('button').find((b) => b.text() === 'Invite a user')!
    await openInviteButton.trigger('click')
    await flushPromises()

    const inviteField = wrapper.find('input[placeholder="Identity id, or display_name#1234"]')
    const inviteForm = wrapper.findAll('form').find((f) => f.text().includes('Send invite'))!

    // Neither a UUID nor a handle: rejected client-side, no network call.
    ;(fetch as ReturnType<typeof vi.fn>).mockClear()
    await inviteField.setValue('some random name')
    await inviteForm.trigger('submit')
    await flushPromises()
    expect(wrapper.text()).toContain("doesn't look like an identity id or a display_name#1234 handle")
    expect(fetch).not.toHaveBeenCalled()

    // A handle is resolved to an identity id before inviting.
    await inviteField.setValue('Nova#4821')
    await inviteForm.trigger('submit')
    await flushPromises()
    expect(wrapper.text()).toContain('Invite sent (id inv1)')
  })

  // Issue #391: channels/events are member-only server-side (403 for a
  // non-member), but that must never blank the whole page — only the
  // member-only tabs should disappear.
  it('renders a recruiting guild for a non-member despite 403s on channels/events', async () => {
    useSessionStore().login('a-token')
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((url: string) => {
        const path = new URL(url, 'http://test').pathname
        const routes: Record<string, unknown> = {
          ...baseRoutes(),
          '/me': { ...profile, identity_id: 'id-outsider' },
          '/guilds/g1': { ...guild, join_policy: 'open', recruiting: true },
        }
        if (path === '/guilds/g1/channels' || path === '/guilds/g1/events') {
          return Promise.resolve({
            ok: false,
            status: 403,
            json: () => Promise.resolve({ error: 'forbidden' }),
            text: () => Promise.resolve('forbidden'),
          })
        }
        const body = path in routes ? routes[path] : undefined
        return Promise.resolve({
          ok: true,
          status: 200,
          json: () => Promise.resolve(body),
          text: () => Promise.resolve(body === undefined ? '' : JSON.stringify(body)),
        })
      }),
    )

    const router = testRouter()
    router.push('/guilds/g1')
    await router.isReady()
    const wrapper = mount(Guild, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Dragon Hunters'))

    // Public info renders despite the 403s.
    expect(wrapper.text()).toContain('Dragon Hunters')
    expect(wrapper.text()).not.toContain('Something went wrong')

    // Member-only tabs are hidden for a non-member rather than shown blank.
    const tabLabels = wrapper.findAll('button').map((b) => b.text())
    expect(tabLabels).not.toContain('Channels')
    expect(tabLabels).not.toContain('Events')
    expect(tabLabels).not.toContain('Calendar')
    expect(tabLabels).toContain('Overview')
    expect(tabLabels).toContain('Members')
  })

  // Issue #393: clicking a member's row opens their read-only profile card.
  // `router.push` is intercepted (rather than letting the navigation
  // actually complete) since this test mounts Guild.vue directly rather
  // than via a <RouterView> — a real route change would otherwise leave
  // this same, still-mounted instance reacting to its own `watch(guildId,
  // load)` with a guild id that's really a user id.
  it('navigates to a member profile card when their row is clicked', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath(baseRoutes())

    const router = testRouter()
    router.push('/guilds/g1')
    await router.isReady()
    const wrapper = mount(Guild, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Dragon Hunters'))
    const pushSpy = vi.spyOn(router, 'push').mockResolvedValue(undefined)

    const membersTab = wrapper.findAll('button').find((b) => b.text() === 'Members')!
    await membersTab.trigger('click')
    await flushPromises()

    const memberButton = wrapper.findAll('button').find((b) => b.text().includes('id-owner'))!
    await memberButton.trigger('click')

    expect(pushSpy).toHaveBeenCalledWith({ name: 'user-profile', params: { id: 'id-owner' } })
  })
})
