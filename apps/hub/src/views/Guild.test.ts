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
import { useSessionStore } from '@avalon/api-client'
import { FakeWebSocket, mockFetchByPath } from '../testing/fakes'

const profile = {
  identity_id: 'id-owner',
  identity_created_at: 'now',
  display_name: 'Nova',
  avatar_url: null,
}

const guild = {
  id: 'g1',
  name: 'Dragon Hunters',
  tag: 'DRGN',
  description: 'A guild.',
  owner: 'id-owner',
  created_at: 'now',
  member_count: 1,
  integrators: [],
  join_policy: 'invite_only',
  motd: null,
  banner: null,
  icon: null,
  links: [],
  recruiting: false,
  public: false,
  game_breakdown_public: false,
  favorite_games: [],
  roster_visibility: 'guild_members',
}

const roles = [
  {
    name_index: 0,
    name: 'owner',
    permissions: ['manage_guild', 'manage_roles', 'manage_members', 'manage_channels'],
    description: '',
    badge: { icon: 'shield', color: 'gray' },
  },
]

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
    '/guilds/g1/integrator-breakdown': { guild_id: 'g1', total_members: 1, breakdown: [] },
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

    // Overview content (integrator history card is always present).
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

  // Issue #464: once the live table's before-cursor pagination is
  // exhausted (a page shorter than the page size — here, one message
  // after a full 50-message first page), "load older" falls through to
  // the archive tier instead of treating that as the end of history.
  it('falls through to the archive tier once live message pagination is exhausted', async () => {
    useSessionStore().login('a-token')

    const liveFirstPage = Array.from({ length: 50 }, (_, i) => ({
      id: `live-${i}`,
      channel_id: 'c1',
      author: 'id-owner',
      body: `live message ${i}`,
      sent_at: new Date(2026, 0, 2, 12, 0, 50 - i).toISOString(),
    }))
    const liveOlderPartial = [
      {
        id: 'live-old-1',
        channel_id: 'c1',
        author: 'id-owner',
        body: 'oldest live message',
        sent_at: new Date(2026, 0, 1, 11, 0).toISOString(),
      },
    ]
    const archivedPage = [
      {
        id: 'archived-1',
        channel_id: 'c1',
        author: 'id-owner',
        body: 'ancient archived message',
        sent_at: new Date(2025, 0, 1).toISOString(),
        archived_at: new Date(2025, 6, 1).toISOString(),
      },
    ]

    let liveMessagesCallCount = 0
    const baseTable: Record<string, unknown> = baseRoutes()
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((url: string, init?: RequestInit) => {
        const path = new URL(url, 'http://test').pathname
        const method = init?.method ?? 'GET'
        let body: unknown

        if (path === '/guilds/g1/channels/c1/messages' && method === 'GET') {
          liveMessagesCallCount += 1
          body = liveMessagesCallCount === 1 ? liveFirstPage : liveOlderPartial
        } else if (path === '/guilds/g1/channels/c1/messages/archive') {
          body = archivedPage
        } else {
          body = path in baseTable ? baseTable[path] : undefined
        }
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

    const channelsTab = wrapper.findAll('button').find((b) => b.text() === 'Channels')!
    await channelsTab.trigger('click')
    await flushPromises()
    await vi.waitFor(() => expect(wrapper.text()).toContain('live message 0'))

    const scrollContainer = wrapper.find('[class*="messageScroll"]')
    expect(scrollContainer.exists()).toBe(true)
    await scrollContainer.trigger('scroll')
    await flushPromises()
    await vi.waitFor(() => expect(wrapper.text()).toContain('ancient archived message'))

    expect(wrapper.text()).toContain('oldest live message')
    expect(wrapper.text()).toContain('Start of channel history.')
  })

  // A channel with nothing in its archive tier behaves exactly as today:
  // once both the live table and the archive both come back short of a
  // full page, pagination stops — no repeated failed/empty request loop.
  it('stops paginating cleanly when a channel has no archived messages either', async () => {
    useSessionStore().login('a-token')

    const baseTable: Record<string, unknown> = baseRoutes()
    let liveMessagesCallCount = 0
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((url: string, init?: RequestInit) => {
        const path = new URL(url, 'http://test').pathname
        const method = init?.method ?? 'GET'
        let body: unknown

        if (path === '/guilds/g1/channels/c1/messages' && method === 'GET') {
          liveMessagesCallCount += 1
          body =
            liveMessagesCallCount === 1
              ? [
                  {
                    id: 'live-1',
                    channel_id: 'c1',
                    author: 'id-owner',
                    body: 'the only message',
                    sent_at: '2026-01-01T00:00:00Z',
                  },
                ]
              : []
        } else if (path === '/guilds/g1/channels/c1/messages/archive') {
          body = []
        } else {
          body = path in baseTable ? baseTable[path] : undefined
        }
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

    const channelsTab = wrapper.findAll('button').find((b) => b.text() === 'Channels')!
    await channelsTab.trigger('click')
    await flushPromises()
    await vi.waitFor(() => expect(wrapper.text()).toContain('the only message'))

    // A single message is not a full page, but there's no way to know yet
    // whether the archive tier holds more — the "load older" affordance
    // stays available until a scroll actually checks.
    expect(wrapper.text()).not.toContain('Start of channel history.')

    const scrollContainer = wrapper.find('[class*="messageScroll"]')
    await scrollContainer.trigger('scroll')
    await flushPromises()
    await vi.waitFor(() => expect(wrapper.text()).toContain('Start of channel history.'))

    const archiveCallsAfterFirstScroll = (fetch as ReturnType<typeof vi.fn>).mock.calls.filter(
      ([url]) => String(url).includes('/messages/archive'),
    ).length
    expect(archiveCallsAfterFirstScroll).toBe(1)

    // hasMoreOlder is now false — a second scroll must not fire another
    // request at all, live or archive.
    await scrollContainer.trigger('scroll')
    await flushPromises()
    const archiveCallsAfterSecondScroll = (fetch as ReturnType<typeof vi.fn>).mock.calls.filter(
      ([url]) => String(url).includes('/messages/archive'),
    ).length
    expect(archiveCallsAfterSecondScroll).toBe(1)
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
      public: false,
      my_rsvp: null,
      details_visible: true,
    }
    mockFetchByPath({
      ...baseRoutes(),
      '/guilds/g1/events': [event],
      '/guilds/g1/events/ev1/rsvps': [
        { identity_id: 'id-owner', status: 'going', responded_at: '2026-09-02T00:00:00Z' },
      ],
      '/identities/profiles': [{ identity_id: 'id-owner', display_name: 'Nova' }],
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
    expect(wrapper.text()).toContain('Nova')
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

  it('creates a role with a description and badge', async () => {
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

    const defineButton = wrapper.findAll('button').find((b) => b.text() === 'Define a role')!
    await defineButton.trigger('click')
    await flushPromises()

    await wrapper.find('input[type="text"]').setValue('raid leader')
    await wrapper.find('input[placeholder="Leads scheduled raids"]').setValue('Leads scheduled raids')
    await wrapper.find('select[aria-label="Badge icon"]').setValue('crown')
    await wrapper.find('select[aria-label="Badge color"]').setValue('gold')

    await wrapper.find('form').trigger('submit')
    await flushPromises()

    const createCall = (fetch as ReturnType<typeof vi.fn>).mock.calls.find(([url, init]) => {
      const method = (init as RequestInit | undefined)?.method
      return String(url).includes('/guilds/g1/roles') && method === 'POST'
    })
    expect(createCall).toBeTruthy()
    const body = JSON.parse((createCall![1] as RequestInit).body as string)
    expect(body.description).toBe('Leads scheduled raids')
    expect(body.badge).toEqual({ icon: 'crown', color: 'gold' })
  })

  it("saves an existing role's description and badge", async () => {
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
    await unlockButton.trigger('click')
    await flushPromises()

    await wrapper.find('input[placeholder="Description"]').setValue('Runs the guild')
    await wrapper.find('select[aria-label="Badge icon"]').setValue('wrench')
    await wrapper.find('select[aria-label="Badge color"]').setValue('blue')

    const saveButton = wrapper.findAll('button').find((b) => b.text() === 'Save')!
    await saveButton.trigger('click')
    await flushPromises()

    const patchCall = (fetch as ReturnType<typeof vi.fn>).mock.calls.find(([url, init]) => {
      const method = (init as RequestInit | undefined)?.method
      return String(url).includes('/guilds/g1/roles/0') && method === 'PATCH'
    })
    expect(patchCall).toBeTruthy()
    const body = JSON.parse((patchCall![1] as RequestInit).body as string)
    expect(body.description).toBe('Runs the guild')
    expect(body.badge).toEqual({ icon: 'wrench', color: 'blue' })
  })

  // Issue #392, updated by #510: the invite field accepts either a raw
  // identity id (a UUID, sent straight through, no resolve call) or a
  // display_name handle (resolved to an identity id first, same as
  // Friends.vue's add-friend flow) — display_name is the handle now, so
  // there's no client-side "does this look like an id or a handle"
  // rejection any more; a handle that doesn't resolve is just whatever
  // error the server returns.
  it('invites by identity id directly, or resolves a handle first', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath({
      ...baseRoutes(),
      '/guilds/g1/invites': { id: 'inv1', guild_id: 'g1', to: 'id-outsider', status: 'pending' },
      '/friends/handle/Nova': { identity_id: '11111111-2222-3333-4444-555555555555' },
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

    const inviteField = wrapper.find('input[placeholder="Identity id, or display name"]')
    const inviteForm = wrapper.findAll('form').find((f) => f.text().includes('Send invite'))!

    // A raw identity id (UUID) is sent straight through — no handle-resolve call.
    ;(fetch as ReturnType<typeof vi.fn>).mockClear()
    await inviteField.setValue('11111111-2222-3333-4444-555555555555')
    await inviteForm.trigger('submit')
    await flushPromises()
    expect(wrapper.text()).toContain('Invite sent — they\'ll see it on their Guilds page.')
    expect(fetch).not.toHaveBeenCalledWith(expect.stringContaining('/friends/handle/'))

    // A handle is resolved to an identity id before inviting.
    await inviteField.setValue('Nova')
    await inviteForm.trigger('submit')
    await flushPromises()
    expect(wrapper.text()).toContain('Invite sent — they\'ll see it on their Guilds page.')
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

  // Issue #448: a non-member of a `public` guild (independent of
  // recruiting, #449) sees a read-only Events tab scoped to that guild's
  // public events, with no RSVP controls.
  it('shows only public events, read-only, to a non-member of a public guild', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath({
      ...baseRoutes(),
      '/me': { ...profile, identity_id: 'id-outsider' },
      '/guilds/g1': { ...guild, public: true },
      '/guilds/g1/members': [],
      '/guilds/g1/events': [
        {
          id: 'e1',
          guild_id: 'g1',
          channel_id: null,
          title: 'Community mixer',
          description: null,
          starts_at: '2026-09-20T20:00:00Z',
          ends_at: null,
          created_by: 'id-owner',
          created_at: 'now',
          rsvp_counts: { going: 0, maybe: 0, not_going: 0 },
          public: true,
          my_rsvp: null,
          details_visible: true,
        },
      ],
    })

    const router = testRouter()
    router.push('/guilds/g1')
    await router.isReady()
    const wrapper = mount(Guild, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Dragon Hunters'))

    const tabLabels = wrapper.findAll('button').map((b) => b.text())
    expect(tabLabels).toContain('Events')
    expect(tabLabels).not.toContain('Channels')

    const eventsTab = wrapper.findAll('button').find((b) => b.text() === 'Events')!
    await eventsTab.trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('Community mixer')
    expect(wrapper.text()).toContain('Join to see everything and RSVP')
    expect(wrapper.findAll('button').some((b) => b.text() === 'Going')).toBe(false)
    expect(wrapper.findAll('button').some((b) => b.text() === 'Edit')).toBe(false)
    expect(wrapper.findAll('button').some((b) => b.text() === 'Delete')).toBe(false)
  })

  // Issue #463: AvalonRsvpControl pre-selects the caller's own RSVP from
  // EventResponse.my_rsvp, and edit/delete are reachable for a manager.
  it("pre-selects the caller's RSVP and allows editing an event", async () => {
    useSessionStore().login('a-token')
    const event = {
      id: 'e1',
      guild_id: 'g1',
      channel_id: null,
      title: 'Raid night',
      description: null,
      starts_at: '2026-09-20T20:00:00Z',
      ends_at: null,
      created_by: 'id-owner',
      created_at: 'now',
      rsvp_counts: { going: 1, maybe: 0, not_going: 0 },
      public: false,
      my_rsvp: 'going',
      details_visible: true,
    }
    mockFetchByPath({ ...baseRoutes(), '/guilds/g1/events': [event] })

    const router = testRouter()
    router.push('/guilds/g1')
    await router.isReady()
    const wrapper = mount(Guild, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Dragon Hunters'))

    const eventsTab = wrapper.findAll('button').find((b) => b.text() === 'Events')!
    await eventsTab.trigger('click')
    await flushPromises()
    await vi.waitFor(() => expect(wrapper.text()).toContain('Raid night'))

    const goingButton = wrapper.findAll('button').find((b) => b.text() === 'Going')!
    expect(goingButton.attributes('aria-pressed')).toBe('true')

    const editButton = wrapper.findAll('button').find((b) => b.text() === 'Edit')!
    await editButton.trigger('click')
    await flushPromises()

    expect((wrapper.find('input[placeholder="Raid night"]').element as HTMLInputElement).value).toBe(
      'Raid night',
    )

    await wrapper.find('input[placeholder="Raid night"]').setValue('Raid night (rescheduled)')
    await wrapper.find('form').trigger('submit')
    await flushPromises()

    const patchCall = (fetch as ReturnType<typeof vi.fn>).mock.calls.find(([url, init]) => {
      const method = (init as RequestInit | undefined)?.method
      return String(url).includes('/guilds/g1/events/e1') && method === 'PATCH'
    })
    expect(patchCall).toBeTruthy()
    const body = JSON.parse((patchCall![1] as RequestInit).body as string)
    expect(body.title).toBe('Raid night (rescheduled)')
  })

  it('deletes an event after confirming', async () => {
    useSessionStore().login('a-token')
    vi.spyOn(window, 'confirm').mockReturnValue(true)
    const event = {
      id: 'e1',
      guild_id: 'g1',
      channel_id: null,
      title: 'Raid night',
      description: null,
      starts_at: '2026-09-20T20:00:00Z',
      ends_at: null,
      created_by: 'id-owner',
      created_at: 'now',
      rsvp_counts: { going: 0, maybe: 0, not_going: 0 },
      public: false,
      my_rsvp: null,
      details_visible: true,
    }
    mockFetchByPath({
      ...baseRoutes(),
      '/guilds/g1/events': [event],
      '/guilds/g1/events/e1': { deleted: true },
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

    const deleteButton = wrapper.findAll('button').find((b) => b.text() === 'Delete')!
    await deleteButton.trigger('click')
    await flushPromises()

    const deleteCall = (fetch as ReturnType<typeof vi.fn>).mock.calls.find(([url, init]) => {
      const method = (init as RequestInit | undefined)?.method
      return String(url).includes('/guilds/g1/events/e1') && method === 'DELETE'
    })
    expect(deleteCall).toBeTruthy()
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

  // Issue #449: recruiting and public are independent settings, each with
  // its own toggle in the Settings tab.
  it('toggles the Public setting independently of Recruiting', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath(baseRoutes())

    const router = testRouter()
    router.push('/guilds/g1')
    await router.isReady()
    const wrapper = mount(Guild, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Dragon Hunters'))

    const settingsTab = wrapper.findAll('button').find((b) => b.text() === 'Settings')!
    await settingsTab.trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain("Anyone signed in can see this guild's roster: no")

    const makePublicButton = wrapper.findAll('button').find((b) => b.text() === 'Make public')!
    await makePublicButton.trigger('click')
    await flushPromises()

    const patchCall = (fetch as ReturnType<typeof vi.fn>).mock.calls.find(([url, init]) => {
      const method = (init as RequestInit | undefined)?.method
      const body = (init as RequestInit | undefined)?.body
      return String(url).includes('/guilds/g1') && method === 'PATCH' && String(body).includes('"public":true')
    })
    expect(patchCall).toBeTruthy()
  })

  it('saves a new roster visibility from the Settings tab', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath(baseRoutes())

    const router = testRouter()
    router.push('/guilds/g1')
    await router.isReady()
    const wrapper = mount(Guild, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('Dragon Hunters'))

    const settingsTab = wrapper.findAll('button').find((b) => b.text() === 'Settings')!
    await settingsTab.trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('Roster visibility')
    const select = wrapper.findAll('select').find((s) => s.text().includes('Anyone'))!
    await select.setValue('private')
    const saveButton = wrapper.findAll('button').find((b) => b.text() === 'Save')!
    await saveButton.trigger('click')
    await flushPromises()

    const patchCall = (fetch as ReturnType<typeof vi.fn>).mock.calls.find(([url, init]) => {
      const method = (init as RequestInit | undefined)?.method
      const body = (init as RequestInit | undefined)?.body
      return (
        String(url).includes('/guilds/g1') &&
        method === 'PATCH' &&
        String(body).includes('"roster_visibility":"private"')
      )
    })
    expect(patchCall).toBeTruthy()
  })
})
