// Rollback view: candidate listing, per-item status, the confirm-then-sign
// reversal flow, and error-code messaging. The SDK-facing wrappers are
// mocked; the pure helpers stay real.
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createMemoryHistory } from 'vue-router'
import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { RollbackCandidate } from '@avalon-initiative/protocol-sdk'
import Rollback from './Rollback.vue'
import { useSessionStore } from '../api/session'
import { mockFetchByPath } from '../testing/fakes'

const candidatesMock = vi.fn()
const reverseMock = vi.fn()

vi.mock('../api/rollback', async (importActual) => ({
  ...(await importActual<typeof import('../api/rollback')>()),
  rollbackCandidates: (...a: unknown[]) => candidatesMock(...a),
  reverseRollbackEvent: (...a: unknown[]) => reverseMock(...a),
}))

function candidate(over: Partial<RollbackCandidate>): RollbackCandidate {
  return {
    eventId: 'e1',
    kind: 'friend.accepted',
    occurredAt: '2026-01-02T00:00:00Z',
    summary: 'Accepted a friend request',
    reversible: true,
    reason: null,
    alreadyReversed: false,
    ...over,
  }
}

const profile = {
  identity_id: 'id-1',
  identity_created_at: 'now',
  display_name: 'Nova',
  avatar_url: null,
}

beforeEach(() => {
  candidatesMock.mockReset()
  reverseMock.mockReset()
  localStorage.clear()
  setActivePinia(createPinia())
})

async function mountView() {
  mockFetchByPath({ '/me': profile })
  localStorage.setItem('avalon:session:token', 'a-token')
  await useSessionStore().initialize()
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [{ path: '/', component: Rollback }],
  })
  router.push('/')
  await router.isReady()
  return mount(Rollback, { global: { plugins: [router] } })
}

async function findActions(wrapper: Awaited<ReturnType<typeof mountView>>) {
  const input = wrapper.find('input')
  await input.setValue('2026-01-01T10:00')
  await flushPromises()
  const find = wrapper.findAll('button').find((b) => b.text() === 'Find actions')!
  await find.trigger('click')
  await flushPromises()
}

function button(wrapper: Awaited<ReturnType<typeof mountView>>, label: string) {
  return wrapper.findAll('button').find((b) => b.text() === label)
}

describe('Rollback view', () => {
  it('lists candidates with a status for each', async () => {
    candidatesMock.mockResolvedValue({
      candidates: [
        candidate({}),
        candidate({
          eventId: 'e2',
          summary: 'Left a friendship',
          reversible: false,
          reason: 'Send a new friend request instead.',
        }),
        candidate({ eventId: 'e3', summary: 'Joined a guild', alreadyReversed: true }),
      ],
    })
    const wrapper = await mountView()
    await findActions(wrapper)
    const text = wrapper.text()
    expect(text).toContain('Accepted a friend request')
    expect(text).toContain('Can undo')
    expect(text).toContain('Cannot undo: Send a new friend request instead.')
    expect(text).toContain('Already undone')
    expect(wrapper.findAll('button').filter((b) => b.text() === 'Undo')).toHaveLength(1)
  })

  it('shows an empty state', async () => {
    candidatesMock.mockResolvedValue({ candidates: [] })
    const wrapper = await mountView()
    await findActions(wrapper)
    expect(wrapper.text()).toContain('No actions were found in that window.')
  })

  it('requires confirmation and reverses with the same since string', async () => {
    candidatesMock
      .mockResolvedValueOnce({ candidates: [candidate({})] })
      .mockResolvedValueOnce({ candidates: [candidate({ alreadyReversed: true })] })
    reverseMock.mockResolvedValue('rev-1')
    const wrapper = await mountView()
    await findActions(wrapper)
    const since = candidatesMock.mock.calls[0][1]

    await button(wrapper, 'Undo')!.trigger('click')
    expect(reverseMock).not.toHaveBeenCalled()
    await button(wrapper, 'Confirm undo')!.trigger('click')
    await flushPromises()

    expect(reverseMock).toHaveBeenCalledWith(expect.anything(), 'e1', since)
    expect(candidatesMock.mock.calls[1][1]).toBe(since)
    expect(wrapper.text()).toContain('Action undone.')
    expect(wrapper.text()).toContain('Already undone')
  })

  it('cancelling the confirmation does not sign anything', async () => {
    candidatesMock.mockResolvedValue({ candidates: [candidate({})] })
    const wrapper = await mountView()
    await findActions(wrapper)
    await button(wrapper, 'Undo')!.trigger('click')
    await button(wrapper, 'Cancel')!.trigger('click')
    expect(reverseMock).not.toHaveBeenCalled()
    expect(button(wrapper, 'Undo')).toBeTruthy()
  })

  it.each([
    ['ROLLBACK_NO_COMPLETED_RECOVERY', 'no completed recovery'],
    ['INVALID_ROLLBACK_WINDOW', 'not valid'],
    ['ROLLBACK_EVENT_NOT_ELIGIBLE', 'not in the window'],
    ['ROLLBACK_NOT_REVERSIBLE', 'cannot be undone'],
    ['ROLLBACK_ALREADY_REVERSED', 'already been undone'],
  ])('maps %s to a plain message when listing', async (code, fragment) => {
    candidatesMock.mockRejectedValue(Object.assign(new Error('x'), { code }))
    const wrapper = await mountView()
    await findActions(wrapper)
    expect(wrapper.find('[role="alert"]').text()).toContain(fragment)
  })

  it('maps a reversal error code and unknown codes generically', async () => {
    candidatesMock.mockResolvedValue({ candidates: [candidate({})] })
    reverseMock.mockRejectedValue(
      Object.assign(new Error('x'), { code: 'ROLLBACK_ALREADY_REVERSED' }),
    )
    const wrapper = await mountView()
    await findActions(wrapper)
    await button(wrapper, 'Undo')!.trigger('click')
    await button(wrapper, 'Confirm undo')!.trigger('click')
    await flushPromises()
    expect(wrapper.find('[role="alert"]').text()).toContain('already been undone')

    candidatesMock.mockRejectedValue(new Error('boom'))
    await button(wrapper, 'Find actions')!.trigger('click')
    await flushPromises()
    expect(wrapper.find('[role="alert"]').text()).toContain('Something went wrong')
  })
})
