// Issue #270's own invariant: the game directory must render games from
// GET /games with no score/ranking element anywhere in the DOM.
import { createRouter, createMemoryHistory } from 'vue-router'
import { mount, flushPromises } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import GameDirectory from './GameDirectory.vue'
import { mockFetchByPath } from '../testing/fakes'

function testRouter() {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/games', component: GameDirectory },
      { path: '/games/:slug', component: GameDirectory },
    ],
  })
}

beforeEach(() => {
  localStorage.clear()
})

describe('GameDirectory', () => {
  it('lists games from GET /games with no score or ranking element anywhere', async () => {
    mockFetchByPath({
      '/games': {
        games: [
          {
            id: 'g1',
            slug: 'ashen-realms',
            name: 'Ashen Realms',
            developer: 'Ashen Studios',
            registered_at: '2026-01-12T00:00:00Z',
            status: 'active',
          },
          {
            id: 'g2',
            slug: 'worldzero',
            name: 'WorldZero',
            developer: 'Lunar Vagabond',
            registered_at: '2026-02-01T00:00:00Z',
            status: 'suspended',
          },
        ],
        next_cursor: null,
      },
    })

    const router = testRouter()
    router.push('/games')
    await router.isReady()
    const wrapper = mount(GameDirectory, { global: { plugins: [router] } })
    await flushPromises()
    await vi.waitFor(() => expect(wrapper.text()).toContain('Ashen Realms'))

    expect(wrapper.text()).toContain('WorldZero')
    expect(wrapper.text()).toContain('suspended')

    const text = wrapper.text().toLowerCase()
    expect(text).not.toContain('score')
    expect(text).not.toContain('rank')
    expect(text).not.toContain('trust')
    expect(text).not.toContain('recommended')
  })

  it('shows an empty message rather than an error when no games match', async () => {
    mockFetchByPath({ '/games': { games: [], next_cursor: null } })

    const router = testRouter()
    router.push('/games')
    await router.isReady()
    const wrapper = mount(GameDirectory, { global: { plugins: [router] } })
    await flushPromises()
    await vi.waitFor(() => expect(wrapper.text()).toContain('No games match'))
  })
})
