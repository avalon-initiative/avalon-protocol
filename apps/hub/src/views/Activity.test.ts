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
    await vi.waitFor(() => expect(wrapper.text()).toContain('You created your identity.'))
  })

  it('falls back to the raw kind for an event this build does not recognize', async () => {
    useSessionStore().login('a-token')
    mockFetchOnce([
      {
        event_id: 'evt-2',
        kind: 'some.future.kind',
        subject: 'identity:id-1:self:test',
        payload: {},
        timestamp: '2026-09-08T00:00:00Z',
      },
    ])

    const wrapper = mount(Activity)
    await vi.waitFor(() => expect(wrapper.text()).toContain('some.future.kind'))
  })

  it('renders sensibly for a fresh identity with no history yet', async () => {
    useSessionStore().login('a-token')
    mockFetchOnce([])

    const wrapper = mount(Activity)
    await vi.waitFor(() => expect(wrapper.text()).toContain('No activity yet'))
  })

  // Issue #389: this view used to load once and never refresh.
  it('polls for new history entries without a manual reload', async () => {
    useSessionStore().login('a-token')
    vi.useFakeTimers()
    mockFetchOnce([])

    const wrapper = mount(Activity)
    await vi.waitFor(() => expect(wrapper.text()).toContain('No activity yet'))

    mockFetchOnce([
      {
        event_id: 'evt-1',
        kind: 'identity.created',
        subject: 'identity:id-1:self:created',
        payload: {},
        timestamp: '2026-09-08T00:00:00Z',
      },
    ])
    await vi.advanceTimersByTimeAsync(15_000)

    expect(wrapper.text()).toContain('You created your identity.')
    vi.useRealTimers()
  })
})
