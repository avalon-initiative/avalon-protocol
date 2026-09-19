// Issue #270's own invariants: every displayed metric carries its
// definition/class label (never a bare number), a suspended/revoked
// status renders visibly distinct from active, and no score/ranking
// element exists anywhere on the page.
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createMemoryHistory } from 'vue-router'
import { mount, flushPromises } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import IntegrationProfile from './IntegrationProfile.vue'
import { useSessionStore } from '@avalon/api-client'
import { mockFetchByPath } from '../testing/fakes'

beforeEach(() => {
  localStorage.clear()
  setActivePinia(createPinia())
})

function testRouter() {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/integrations/:slug', name: 'integration-profile', component: IntegrationProfile },
      { path: '/connect/:slug', name: 'connect-integrator', component: IntegrationProfile },
      { path: '/guilds', name: 'guilds', component: IntegrationProfile },
      { path: '/guilds/:id', name: 'guild', component: IntegrationProfile },
    ],
  })
}

const registryResponse = {
  players: { value: 10, definition: 'Distinct identities with an active IntegratorBinding.', class: 'durable-derived' },
  total_players_ever: { value: 12, definition: 'Distinct identities that ever had a binding.', class: 'durable-derived' },
  achievements_issued: { value: 5, definition: 'Count of achievement.issued events by this issuer.', class: 'durable-derived' },
  achievements_revoked: { value: 1, definition: 'Count of achievement.revoked events by this issuer.', class: 'durable-derived' },
  unique_achievement_holders: {
    value: 4,
    definition: 'Distinct subjects with at least one valid attestation from this issuer.',
    class: 'durable-derived',
  },
}

async function mountProfile(integratorOverrides: Record<string, unknown> = {}) {
  mockFetchByPath({
    '/integrations/ashen-realms': {
      id: 'g1',
      slug: 'ashen-realms',
      name: 'Ashen Realms',
      owner_name: 'Ashen Studios',
      registered_at: '2026-01-12T00:00:00Z',
      status: 'active',
      requested_capabilities: [],
      ...integratorOverrides,
    },
    '/integrations/ashen-realms/registry': registryResponse,
    '/integrations/ashen-realms/keys': [],
  })

  const router = testRouter()
  router.push('/integrations/ashen-realms')
  await router.isReady()
  const wrapper = mount(IntegrationProfile, { global: { plugins: [router] } })
  await flushPromises()
  await vi.waitFor(() => expect(wrapper.text()).toContain('Ashen Realms'))
  return wrapper
}

describe('IntegrationProfile', () => {
  it('renders every metric with its definition and class label, never a bare number', async () => {
    const wrapper = await mountProfile()
    const text = wrapper.text()

    expect(text).toContain('Players')
    expect(text).toContain('Distinct identities with an active IntegratorBinding.')
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

  it('shows issuer key history including revoked keys', async () => {
    mockFetchByPath({
      '/integrations/ashen-realms': {
        id: 'g1',
        slug: 'ashen-realms',
        name: 'Ashen Realms',
        owner_name: 'Ashen Studios',
        registered_at: '2026-01-12T00:00:00Z',
        status: 'active',
        requested_capabilities: [],
      },
      '/integrations/ashen-realms/registry': registryResponse,
      '/integrations/ashen-realms/keys': [
        { key_id: 'k1', algorithm: 'ed25519', role: 'root', valid_from: '2026-01-12T00:00:00Z', valid_until: null, revoked_at: null },
        {
          key_id: 'k2',
          algorithm: 'ed25519',
          role: 'operational',
          valid_from: '2026-02-01T00:00:00Z',
          valid_until: null,
          revoked_at: '2026-03-01T00:00:00Z',
        },
      ],
    })
    const router = testRouter()
    router.push('/integrations/ashen-realms')
    await router.isReady()
    const wrapper = mount(IntegrationProfile, { global: { plugins: [router] } })
    await flushPromises()
    await vi.waitFor(() => expect(wrapper.text()).toContain('Ashen Realms'))

    expect(wrapper.text()).toContain('root')
    expect(wrapper.text()).toContain('operational')
    expect(wrapper.text()).toContain('Revoked')
  })

  it('does not show "Your access" for a logged-out visitor', async () => {
    const wrapper = await mountProfile()
    expect(wrapper.text()).not.toContain('Your access')
  })

  // Issue #467.
  it("reaches ConnectIntegration.vue for this slug when the caller hasn't connected", async () => {
    useSessionStore().login('a-token')
    mockFetchByPath({
      '/integrations/ashen-realms': {
        id: 'g1',
        slug: 'ashen-realms',
        name: 'Ashen Realms',
        owner_name: 'Ashen Studios',
        registered_at: '2026-01-12T00:00:00Z',
        status: 'active',
        requested_capabilities: [],
      },
      '/integrations/ashen-realms/registry': registryResponse,
      '/integrations/ashen-realms/keys': [],
      '/me/connections': [],
      '/guilds/discover': { guilds: [], next_cursor: null },
    })

    const router = testRouter()
    router.push('/integrations/ashen-realms')
    await router.isReady()
    const wrapper = mount(IntegrationProfile, { global: { plugins: [router] } })
    await flushPromises()
    await vi.waitFor(() => expect(wrapper.text()).toContain("You haven't connected to this integrator."))

    const connectButton = wrapper.findAll('button').find((b) => b.text() === 'Connect')!
    await connectButton.trigger('click')
    await flushPromises()

    expect(router.currentRoute.value.name).toBe('connect-integrator')
    expect(router.currentRoute.value.params.slug).toBe('ashen-realms')
  })

  it('links "Guilds playing this" through to Discover, pre-filtered to this integrator', async () => {
    useSessionStore().login('a-token')
    mockFetchByPath({
      '/integrations/ashen-realms': {
        id: 'g1',
        slug: 'ashen-realms',
        name: 'Ashen Realms',
        owner_name: 'Ashen Studios',
        registered_at: '2026-01-12T00:00:00Z',
        status: 'active',
        requested_capabilities: [],
      },
      '/integrations/ashen-realms/registry': registryResponse,
      '/integrations/ashen-realms/keys': [],
      '/me/connections': [],
      '/guilds/discover': {
        guilds: [
          {
            id: 'guild-1',
            name: 'Dragon Hunters',
            tag: 'DRGN',
            description: 'Raiders.',
            recruiting: true,
            member_count: 10,
            created_at: 'now',
            banner: null,
            icon: null,
          },
        ],
        next_cursor: null,
      },
    })

    const router = testRouter()
    router.push('/integrations/ashen-realms')
    await router.isReady()
    const wrapper = mount(IntegrationProfile, { global: { plugins: [router] } })
    await flushPromises()
    await vi.waitFor(() => expect(wrapper.text()).toContain('Dragon Hunters'))

    const discoverCall = (fetch as ReturnType<typeof vi.fn>).mock.calls.find(([url]) =>
      String(url).includes('/guilds/discover'),
    )
    expect(discoverCall).toBeTruthy()
    expect(String(discoverCall![0])).toContain('game=ashen-realms')

    const seeAllButton = wrapper.findAll('button').find((b) => b.text() === 'See all in Discover')!
    await seeAllButton.trigger('click')
    await flushPromises()

    expect(router.currentRoute.value.name).toBe('guilds')
    expect(router.currentRoute.value.query.integrator).toBe('ashen-realms')
  })
})
