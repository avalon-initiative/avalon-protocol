// Issue #246: the "My guilds" list threads GuildResponse.icon into
// AvalonGuildCard's iconUrl prop, rendered as a small badge image — falls
// back to the card's existing text-only rendering when a guild has no icon
// set.
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createMemoryHistory } from 'vue-router'
import { mount, flushPromises } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import Guilds from './Guilds.vue'
import { useSessionStore } from '../stores/session'
import { mockFetchByPath } from '../testing/fakes'

function testRouter() {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/guilds', component: Guilds },
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
  games: [],
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
      '/me/guilds': [
        { guild_id: 'g1', role_index: 0, joined_at: 'now' },
        { guild_id: 'g2', role_index: 0, joined_at: 'now' },
      ],
      '/guilds/g1': { ...guildBase, id: 'g1', icon: 'https://example.com/icon.png' },
      '/guilds/g2': { ...guildBase, id: 'g2', name: 'Silent Order', tag: 'SILO', icon: null },
    })

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
})
