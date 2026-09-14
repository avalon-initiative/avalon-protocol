// Issue #270's own invariant: the integrator directory must render integrators from
// GET /integrations with no score/ranking element anywhere in the DOM.
import { createRouter, createMemoryHistory } from 'vue-router'
import { mount, flushPromises } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import IntegrationDirectory from './IntegrationDirectory.vue'
import { mockFetchByPath } from '../testing/fakes'

function testRouter() {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/integrations', component: IntegrationDirectory },
      { path: '/integrations/:slug', name: 'integration-profile', component: IntegrationDirectory },
    ],
  })
}

beforeEach(() => {
  localStorage.clear()
})

describe('IntegrationDirectory', () => {
  it('lists integrators from GET /integrations with no score or ranking element anywhere', async () => {
    mockFetchByPath({
      '/integrations': {
        integrators: [
          {
            id: 'g1',
            slug: 'ashen-realms',
            name: 'Ashen Realms',
            owner_name: 'Ashen Studios',
            registered_at: '2026-01-12T00:00:00Z',
            status: 'active',
            category: 'game',
          },
          {
            id: 'g2',
            slug: 'worldzero',
            name: 'WorldZero',
            owner_name: 'Lunar Vagabond',
            registered_at: '2026-02-01T00:00:00Z',
            status: 'suspended',
            category: 'game',
          },
        ],
        next_cursor: null,
      },
    })

    const router = testRouter()
    router.push('/integrations')
    await router.isReady()
    const wrapper = mount(IntegrationDirectory, { global: { plugins: [router] } })
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

  it('shows an empty message rather than an error when no integrators match', async () => {
    mockFetchByPath({ '/integrations': { integrators: [], next_cursor: null } })

    const router = testRouter()
    router.push('/integrations')
    await router.isReady()
    const wrapper = mount(IntegrationDirectory, { global: { plugins: [router] } })
    await flushPromises()
    await vi.waitFor(() => expect(wrapper.text()).toContain('No integrators match'))
  })

  it('renders the Apps/Services tabs as empty rather than hiding them', async () => {
    mockFetchByPath({
      '/integrations': {
        integrators: [
          {
            id: 'g1',
            slug: 'ashen-realms',
            name: 'Ashen Realms',
            owner_name: 'Ashen Studios',
            registered_at: '2026-01-12T00:00:00Z',
            status: 'active',
            category: 'game',
          },
        ],
        next_cursor: null,
      },
    })

    const router = testRouter()
    router.push('/integrations')
    await router.isReady()
    const wrapper = mount(IntegrationDirectory, { global: { plugins: [router] } })
    await flushPromises()
    await vi.waitFor(() => expect(wrapper.text()).toContain('Ashen Realms'))

    const tabs = wrapper.findAll('[role="tab"]')
    expect(tabs.map((t) => t.text())).toEqual(['Integrators', 'Apps', 'Services'])

    await tabs[1].trigger('click')
    await flushPromises()
    expect(wrapper.text()).not.toContain('Ashen Realms')
    expect(wrapper.text()).toContain('No apps match')
  })
})
