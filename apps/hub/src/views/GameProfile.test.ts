// Issue #270's own invariants: every displayed metric carries its
// definition/class label (never a bare number), a suspended/revoked
// status renders visibly distinct from active, and no score/ranking
// element exists anywhere on the page.
import { createRouter, createMemoryHistory } from 'vue-router'
import { mount, flushPromises } from '@vue/test-utils'
import { describe, expect, it, vi } from 'vitest'
import GameProfile from './GameProfile.vue'
import { mockFetchByPath } from '../testing/fakes'

function testRouter() {
  return createRouter({
    history: createMemoryHistory(),
    routes: [{ path: '/games/:slug', component: GameProfile }],
  })
}

const registryResponse = {
  players: { value: 10, definition: 'Distinct identities with an active GameBinding.', class: 'durable-derived' },
  total_players_ever: { value: 12, definition: 'Distinct identities that ever had a binding.', class: 'durable-derived' },
  achievements_issued: { value: 5, definition: 'Count of achievement.issued events by this issuer.', class: 'durable-derived' },
  achievements_revoked: { value: 1, definition: 'Count of achievement.revoked events by this issuer.', class: 'durable-derived' },
  unique_achievement_holders: {
    value: 4,
    definition: 'Distinct subjects with at least one valid attestation from this issuer.',
    class: 'durable-derived',
  },
}

async function mountProfile(gameOverrides: Record<string, unknown> = {}) {
  mockFetchByPath({
    '/games/ashen-realms': {
      id: 'g1',
      slug: 'ashen-realms',
      name: 'Ashen Realms',
      developer: 'Ashen Studios',
      registered_at: '2026-01-12T00:00:00Z',
      status: 'active',
      requested_capabilities: [],
      ...gameOverrides,
    },
    '/games/ashen-realms/registry': registryResponse,
  })

  const router = testRouter()
  router.push('/games/ashen-realms')
  await router.isReady()
  const wrapper = mount(GameProfile, { global: { plugins: [router] } })
  await flushPromises()
  await vi.waitFor(() => expect(wrapper.text()).toContain('Ashen Realms'))
  return wrapper
}

describe('GameProfile', () => {
  it('renders every metric with its definition and class label, never a bare number', async () => {
    const wrapper = await mountProfile()
    const text = wrapper.text()

    expect(text).toContain('Players')
    expect(text).toContain('Distinct identities with an active GameBinding.')
    expect(text).toContain('Achievements issued')
    expect(text).toContain('Count of achievement.issued events by this issuer.')
    expect(text).toContain('Unique achievement holders')
    // Every metric tile also carries its class label.
    expect(text.match(/durable-derived/g)?.length).toBe(5)
  })

  it('has no score or ranking element anywhere on the page', async () => {
    const wrapper = await mountProfile()
    const text = wrapper.text().toLowerCase()
    expect(text).not.toContain('score')
    expect(text).not.toContain('rank')
    expect(text).not.toContain('recommended')
  })

  it('marks a non-active status visibly distinct from active', async () => {
    const active = await mountProfile({ status: 'active' })
    // No status badge is rendered at all for the default "active" state.
    expect(active.find('[class*="statusBadge"]').exists()).toBe(false)

    const suspended = await mountProfile({ status: 'suspended' })
    expect(suspended.find('[class*="statusBadge"]').exists()).toBe(true)
    expect(suspended.text()).toContain('suspended')
  })
})
