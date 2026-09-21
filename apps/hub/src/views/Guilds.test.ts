// Issue #246: the "My guilds" list threads GuildResponse.icon into
// AvalonGuildCard's iconUrl prop, rendered as a small badge image — falls
// back to the card's existing text-only rendering when a guild has no icon
// set.
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createMemoryHistory } from 'vue-router'
import { mount, flushPromises } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import Guilds from './Guilds.vue'
import { useSessionStore } from '@avalon/api-client'
import { useSessionStore as useNewSessionStore } from '../api/session'
import { mockFetchByPath } from '../testing/fakes'

function testRouter() {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/guilds', name: 'guilds', component: Guilds },
      { path: '/guilds/:id', component: Guilds },
    ],
  })
}

const guildBase = {
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
  links: [],
  recruiting: false,
  game_breakdown_public: false,
  favorite_games: [],
}

beforeEach(() => {
  localStorage.clear()
  setActivePinia(createPinia())
})

describe('Guilds', () => {
  it('renders the icon badge for a guild with an icon set, none for one without', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath({
      '/me': { identity_id: 'id-1', identity_created_at: 'now', display_name: 'Nova', avatar_url: null },
      '/me/guilds': [
        { guild_id: 'g1', role_index: 0, joined_at: 'now' },
        { guild_id: 'g2', role_index: 0, joined_at: 'now' },
      ],
      '/guilds/g1': { ...guildBase, id: 'g1', icon: 'https://example.com/icon.png' },
      '/guilds/g2': { ...guildBase, id: 'g2', name: 'Silent Order', tag: 'SILO', icon: null },
    })
    // useMyGuilds (Guilds.vue's "My guilds" list) reads its bearer token
    // from the new @avalon/sdk session store (#712) — HubShell/Guilds.vue
    // itself hasn't migrated yet, so this test still also logs into the
    // old store above for whatever else in Guilds.vue still depends on it.
    localStorage.setItem('avalon:session:token', 'a-token')
    await useNewSessionStore().initialize()

    const router = testRouter()
    router.push('/guilds')
    await router.isReady()
    const wrapper = mount(Guilds, { global: { plugins: [router] } })
    await flushPromises()
    await vi.waitFor(() => expect(wrapper.text()).toContain('Dragon Hunters'))

    const images = wrapper.findAll('img')
    expect(images).toHaveLength(1)
    expect(images[0].attributes('src')).toBe('https://example.com/icon.png')
    expect(wrapper.text()).toContain('Silent Order')
  })

  it('surfaces a pending guild invite and accepts it (issue #442)', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath({
      '/me/guilds': [],
      '/me/guild-invites': [
        { id: 'inv1', guild_id: 'g1', guild_name: 'Dragon Hunters', from: 'id-owner', created_at: 'now' },
      ],
      '/identities/profiles': [
        { identity_id: 'id-owner', display_name: 'Nova', avatar_url: null },
      ],
      '/guilds/g1/invites/inv1/accept': { guild_id: 'g1', identity_id: 'id-1', role_index: 1, joined_at: 'now' },
    })

    const router = testRouter()
    router.push('/guilds')
    await router.isReady()
    const wrapper = mount(Guilds, { global: { plugins: [router] } })
    await flushPromises()
    await vi.waitFor(() => expect(wrapper.text()).toContain('invited you to'))
    expect(wrapper.text()).toContain('Nova')
    expect(wrapper.text()).toContain('Dragon Hunters')

    const acceptButton = wrapper.findAll('button').find((b) => b.text() === 'Accept')!
    await acceptButton.trigger('click')
    await flushPromises()

    const acceptCall = (fetch as ReturnType<typeof vi.fn>).mock.calls.find(([url]) =>
      String(url).includes('/guilds/g1/invites/inv1/accept'),
    )
    expect(acceptCall).toBeTruthy()
  })

  // Issue #467: a deep link from IntegrationProfile.vue's "Guilds playing
  // this" opens straight into Discover, pre-filtered to that integrator.
  it('opens the Discover tab pre-filtered when landing with ?integrator=', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath({
      '/me/guilds': [],
      '/guilds/discover': {
        guilds: [{ ...guildBase, id: 'g1', name: 'Dragon Hunters' }],
        next_cursor: null,
      },
    })

    const router = testRouter()
    router.push('/guilds?integrator=ashen-realms')
    await router.isReady()
    const wrapper = mount(Guilds, { global: { plugins: [router] } })
    await flushPromises()
    await vi.waitFor(() => expect(wrapper.text()).toContain('Dragon Hunters'))

    expect(wrapper.text()).toContain('Filtered to guilds playing')
    expect(wrapper.text()).toContain('ashen-realms')

    const discoverCall = (fetch as ReturnType<typeof vi.fn>).mock.calls.find(([url]) =>
      String(url).includes('/guilds/discover'),
    )
    expect(discoverCall).toBeTruthy()
    expect(String(discoverCall![0])).toContain('game=ashen-realms')

    const clearButton = wrapper.findAll('button').find((b) => b.text() === 'Clear')!
    await clearButton.trigger('click')
    await flushPromises()
    expect(wrapper.text()).not.toContain('Filtered to guilds playing')
  })

  // Issue #432/ADR #437: a tier-2 browse view — should pick up a newly
  // created public guild without a manual reload, but only once the
  // Discover tab is actually opened (it's lazy-loaded).
  it('polls the Discover board for newly created guilds once opened', async () => {
    useSessionStore().login('a-token')
    vi.useFakeTimers()
    let call = 0
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((url: string) => {
        const path = new URL(url, 'http://test').pathname
        let body: unknown
        if (path === '/me/guilds') {
          body = []
        } else if (path === '/guilds/discover') {
          call += 1
          body =
            call === 1
              ? { guilds: [], next_cursor: null }
              : { guilds: [{ ...guildBase, id: 'g1', name: 'Dragon Hunters' }], next_cursor: null }
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

    const router = testRouter()
    router.push('/guilds')
    await router.isReady()
    const wrapper = mount(Guilds, { global: { plugins: [router] } })
    await flushPromises()

    const discoverTab = wrapper.findAll('button').find((b) => b.text() === 'Discover')!
    await discoverTab.trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('No guilds match your search.')

    await vi.advanceTimersByTimeAsync(5 * 60_000)
    await flushPromises()
    expect(wrapper.text()).toContain('Dragon Hunters')

    vi.useRealTimers()
  })
})
