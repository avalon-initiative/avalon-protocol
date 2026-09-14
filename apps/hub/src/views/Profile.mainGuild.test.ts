// No ticket — see avalon_protocol::identity::Profile::main_guild's doc
// comment. Mirrors Profile.selfDescription.test.ts's own pattern; the
// guild dropdown reuses useMyGuilds (GET /me/guilds + GET /guilds/{id}
// per membership), same data source Guilds.vue's landing page already
// fetches, so it can only ever offer guilds the caller is actually a
// member of.
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createMemoryHistory } from 'vue-router'
import { mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import Profile from './Profile.vue'
import { useSessionStore } from '../stores/session'
import { mockFetchByPath } from '../testing/fakes'

const baseProfile = {
  identity_id: 'id-1',
  identity_created_at: 'now',
  display_name: 'Nova',
  avatar_url: null,
  handle: 'Nova#4821',
  bio: null,
  pronouns: null,
  favorite_genres: [],
  banner_url: null,
  status: null,
  links: [],
  timezone: null,
  theme_color: null,
  location: null,
  main_guild: null,
  effective_main_guild: null,
  discoverable: false,
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
  icon: null,
  links: [],
  recruiting: false,
  game_breakdown_public: false,
  favorite_games: [],
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

describe('Profile main guild field', () => {
  it('only offers guilds the caller is actually a member of', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath({
      '/me': baseProfile,
      '/me/passkeys': [],
      '/me/guilds': [
        { guild_id: 'g1', role_index: 0, joined_at: '2026-01-01T00:00:00Z' },
        { guild_id: 'g2', role_index: 0, joined_at: '2026-01-02T00:00:00Z' },
      ],
      '/guilds/g1': { ...guildBase, id: 'g1', name: 'Dragon Hunters' },
      '/guilds/g2': { ...guildBase, id: 'g2', name: 'Silent Order', tag: 'SILO' },
    })

    const router = testRouter()
    router.push('/')
    await router.isReady()
    const wrapper = mount(Profile, { global: { plugins: [router] } })

    await vi.waitFor(() => expect(wrapper.text()).toContain('Main guild'))
    const select = await vi.waitFor(() => {
      const el = wrapper.find('select')
      expect(el.exists()).toBe(true)
      return el
    })
    const optionTexts = select.findAll('option').map((o) => o.text().trim())
    expect(optionTexts).toEqual(['No main guild set', 'Dragon Hunters', 'Silent Order'])
  })

  it('shows the earliest-joined guild as the effective default when unset', async () => {
    useSessionStore().login('a-token')
    // GET /me itself reports the server-computed effective default.
    mockFetchByPath({
      '/me': { ...baseProfile, effective_main_guild: 'g1' },
      '/me/passkeys': [],
      '/me/guilds': [{ guild_id: 'g1', role_index: 0, joined_at: '2026-01-01T00:00:00Z' }],
      '/guilds/g1': { ...guildBase, id: 'g1', name: 'Dragon Hunters' },
    })

    const router = testRouter()
    router.push('/')
    await router.isReady()
    const wrapper = mount(Profile, { global: { plugins: [router] } })

    await vi.waitFor(() => expect(wrapper.text()).toContain('Dragon Hunters'))
  })

  /**
   * Like Profile.selfDescription.test.ts's mockMeSequence, `/me` needs to
   * answer differently for the initial GET versus the PATCH a save sends.
   */
  function mockMeSequence(bodies: unknown[]) {
    let call = 0
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((url: string) => {
        const path = new URL(url, 'http://test').pathname
        let body: unknown
        if (path === '/me') {
          body = bodies[Math.min(call, bodies.length - 1)]
          call += 1
        } else if (path === '/me/passkeys') {
          body = []
        } else if (path === '/me/guilds') {
          body = [{ guild_id: 'g1', role_index: 0, joined_at: '2026-01-01T00:00:00Z' }]
        } else if (path === '/guilds/g1') {
          body = { ...guildBase, id: 'g1', name: 'Dragon Hunters' }
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

  it('saving a selected main guild reflects the server response', async () => {
    mockMeSequence([
      baseProfile,
      { ...baseProfile, main_guild: 'g1', effective_main_guild: 'g1' },
    ])
    useSessionStore().login('a-token')

    const router = testRouter()
    router.push('/')
    await router.isReady()
    const wrapper = mount(Profile, { global: { plugins: [router] } })
    await vi.waitFor(() => expect(wrapper.find('select').exists()).toBe(true))

    await wrapper.find('select').setValue('g1')
    const saveButton = wrapper.findAll('button').find((b) => b.text().includes('Save main guild'))
    await saveButton!.trigger('click')

    await vi.waitFor(() => {
      expect((wrapper.find('select').element as HTMLSelectElement).value).toBe('g1')
    })
  })
})
