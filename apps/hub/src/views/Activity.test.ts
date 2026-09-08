// Issue #121 acceptance criteria: the view renders the caller's own
// activity list, and an empty list (a fresh identity, only
// `identity.created` pending in the outbox) renders sensibly rather than a
// blank page.
import { createPinia, setActivePinia } from 'pinia'
import { mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import Activity from './Activity.vue'
import { useSessionStore } from '../stores/session'

function mockFetchOnce(body: unknown) {
  vi.stubGlobal(
    'fetch',
    vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: () => Promise.resolve(body),
      text: () => Promise.resolve(JSON.stringify(body)),
    }),
  )
}

beforeEach(() => {
  localStorage.clear()
  setActivePinia(createPinia())
})

describe('Activity', () => {
  it('renders the event history returned by GET /me/history', async () => {
    useSessionStore().login('a-token')
    mockFetchOnce([
      {
        event_id: 'evt-1',
        kind: 'identity.created',
        subject: 'identity:id-1:self:created',
        payload: {},
        timestamp: '2026-09-08T00:00:00Z',
      },
    ])

    const wrapper = mount(Activity)
    await vi.waitFor(() => expect(wrapper.text()).toContain('identity.created'))
  })

  it('renders sensibly for a fresh identity with no history yet', async () => {
    useSessionStore().login('a-token')
    mockFetchOnce([])

    const wrapper = mount(Activity)
    await vi.waitFor(() => expect(wrapper.text()).toContain('No activity yet'))
  })
})
