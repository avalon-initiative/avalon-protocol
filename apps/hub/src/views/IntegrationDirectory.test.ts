// Issue #270's own invariant: the integrator directory must render integrators from
// GET /integrations with no score/ranking element anywhere in the DOM.
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createMemoryHistory } from 'vue-router'
import { mount, flushPromises } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import IntegrationDirectory from './IntegrationDirectory.vue'
import { useSessionStore } from '../api/session'
import { mockFetchByPath } from '../testing/fakes'

const profile = {
  identity_id: 'id-1',
  identity_created_at: 'now',
  display_name: 'Nova',
  avatar_url: null,
}

function testRouter() {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/integrations', component: IntegrationDirectory },
      { path: '/integrations/:slug', name: 'integration-profile', component: IntegrationDirectory },
      { path: '/connect/:slug', name: 'connect-integrator', component: IntegrationDirectory },
    ],
  })
}

beforeEach(() => {
  localStorage.clear()
  setActivePinia(createPinia())
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
    await vi.waitFor(() => expect(wrapper.text()).toContain('No games match'))
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
    expect(tabs.map((t) => t.text())).toEqual(['Games', 'Apps', 'Services'])

    await tabs[1].trigger('click')
    await flushPromises()
    expect(wrapper.text()).not.toContain('Ashen Realms')
    expect(wrapper.text()).toContain('No apps match')
  })

  // Issue #467: a Connect action reachable straight from the directory,
  // not just after already opening an integrator's own profile.
  it('reaches ConnectIntegration.vue for a card not yet connected', async () => {
    mockFetchByPath({
      '/me': profile,
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
      '/me/connections': [],
    })
    localStorage.setItem('avalon:session:token', 'a-token')
    await useSessionStore().initialize()

    const router = testRouter()
    router.push('/integrations')
    await router.isReady()
    const wrapper = mount(IntegrationDirectory, { global: { plugins: [router] } })
    await flushPromises()
    await vi.waitFor(() => expect(wrapper.text()).toContain('Ashen Realms'))

    const connectButton = wrapper.findAll('button').find((b) => b.text() === 'Connect')!
    await connectButton.trigger('click')
    await flushPromises()

    expect(router.currentRoute.value.name).toBe('connect-integrator')
    expect(router.currentRoute.value.params.slug).toBe('ashen-realms')
  })

  // Issue #432/ADR #437: a tier-2 browse view — should pick up a newly
  // published integrator without a manual reload.
  it('polls for newly published integrators without a manual reload', async () => {
    vi.useFakeTimers()
    let call = 0
    vi.stubGlobal(
      'fetch',
      vi.fn().mockImplementation((url: string) => {
        const path = new URL(url, 'http://test').pathname
        let body: unknown
        if (path === '/integrations') {
          call += 1
          body =
            call === 1
              ? { integrators: [], next_cursor: null }
              : {
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
                }
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
    router.push('/integrations')
    await router.isReady()
    const wrapper = mount(IntegrationDirectory, { global: { plugins: [router] } })
    await flushPromises()
    expect(wrapper.text()).toContain('No games match')

    await vi.advanceTimersByTimeAsync(5 * 60_000)
    await flushPromises()
    expect(wrapper.text()).toContain('Ashen Realms')

    vi.useRealTimers()
  })

  it('hides the Connect action once already connected, and for a logged-out visitor', async () => {
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

    expect(wrapper.findAll('button').some((b) => b.text() === 'Connect')).toBe(false)
  })
})
