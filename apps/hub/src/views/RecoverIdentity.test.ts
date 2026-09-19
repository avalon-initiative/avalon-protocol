// Issue #394: the recovery flow used to dead-end once a request reached
// `delay` status past its `delay_ends_at` — there was no way to finalize it
// from the Hub. This covers the "Finish recovery" action appearing only
// once ready, and that finalizing signs the device in via a normal login.
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createMemoryHistory } from 'vue-router'
import { mount, flushPromises } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { AuthenticationResponseJSON, RegistrationResponseJSON } from '@simplewebauthn/browser'

const runRegistrationCeremonyMock = vi.fn<(options: unknown) => Promise<RegistrationResponseJSON>>()
const runAuthenticationCeremonyMock = vi.fn<(options: unknown) => Promise<AuthenticationResponseJSON>>()

// This view's `login`/`startRecovery` calls reach the WebAuthn ceremony
// through @avalon/api-client's *internal* ./crypto/webauthn import (identity.ts
// calls it directly, not via the package's own barrel) — mocking that
// resolved submodule, rather than the @avalon/api-client entrypoint, is what
// actually intercepts it.
vi.mock('@avalon/api-client/src/crypto/webauthn', () => ({
  runRegistrationCeremony: (options: unknown) => runRegistrationCeremonyMock(options),
  runAuthenticationCeremony: (options: unknown) => runAuthenticationCeremonyMock(options),
}))

import RecoverIdentity from './RecoverIdentity.vue'
import { useSessionStore } from '@avalon/api-client'

const requestBase = {
  id: 'req1',
  identity_id: 'id-1',
  threshold: 2,
  approvals_count: 2,
  requested_at: '2026-01-01T00:00:00Z',
}

function testRouter() {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/recover-identity', name: 'recover-identity', component: RecoverIdentity },
      { path: '/login', name: 'login', component: RecoverIdentity },
      { path: '/', name: 'home', component: RecoverIdentity },
    ],
  })
}

function mockFetchByPath(responses: Record<string, unknown>) {
  vi.stubGlobal(
    'fetch',
    vi.fn().mockImplementation((url: string) => {
      const path = new URL(url, 'http://test').pathname
      const body = path in responses ? responses[path] : undefined
      return Promise.resolve({
        ok: true,
        status: 200,
        json: () => Promise.resolve(body),
        text: () => Promise.resolve(body === undefined ? '' : JSON.stringify(body)),
      })
    }),
  )
}

beforeEach(() => {
  setActivePinia(createPinia())
  runRegistrationCeremonyMock.mockReset()
  runAuthenticationCeremonyMock.mockReset()
})

afterEach(() => {
  vi.unstubAllGlobals()
})

async function startRecovery(wrapper: ReturnType<typeof mount>) {
  await wrapper.find('input').setValue('id-1')
  await wrapper.find('form').trigger('submit')
  await flushPromises()
}

describe('RecoverIdentity', () => {
  it('does not show "Finish recovery" while still collecting approvals or waiting out the delay', async () => {
    runRegistrationCeremonyMock.mockResolvedValue({} as RegistrationResponseJSON)
    mockFetchByPath({
      '/recovery/requests/start': { ticket_id: 't1', challenge: { publicKey: {} } },
      '/recovery/requests/finish': { ...requestBase, status: 'pending_approvals', delay_ends_at: null },
    })

    const router = testRouter()
    router.push('/recover-identity')
    await router.isReady()
    const wrapper = mount(RecoverIdentity, { global: { plugins: [router] } })
    await startRecovery(wrapper)

    expect(wrapper.text()).toContain('pending_approvals')
    expect(wrapper.text()).not.toContain('Finish recovery')
  })

  it('shows "Finish recovery" once delay has elapsed, and signs the device in on click', async () => {
    runRegistrationCeremonyMock.mockResolvedValue({} as RegistrationResponseJSON)
    runAuthenticationCeremonyMock.mockResolvedValue({} as AuthenticationResponseJSON)
    const pastDelay = new Date(Date.now() - 60_000).toISOString()
    mockFetchByPath({
      '/recovery/requests/start': { ticket_id: 't1', challenge: { publicKey: {} } },
      '/recovery/requests/finish': { ...requestBase, status: 'delay', delay_ends_at: pastDelay },
      '/recovery/requests/req1/finalize': { ...requestBase, status: 'completed', delay_ends_at: pastDelay },
      '/sessions/start': { ticket_id: 't2', challenge: { publicKey: {} } },
      '/sessions/finish': { token: 'new-session-token', expires_at: '2027-01-01T00:00:00Z' },
    })

    const router = testRouter()
    router.push('/recover-identity')
    await router.isReady()
    const wrapper = mount(RecoverIdentity, { global: { plugins: [router] } })
    await startRecovery(wrapper)

    const finishButton = wrapper.findAll('button').find((b) => b.text().includes('Finish recovery'))
    expect(finishButton).toBeTruthy()
    await finishButton!.trigger('click')
    await flushPromises()

    expect(useSessionStore().token).toBe('new-session-token')
    expect(router.currentRoute.value.name).toBe('home')
  })

  it('does not show "Finish recovery" for a delay request whose delay has not elapsed yet', async () => {
    runRegistrationCeremonyMock.mockResolvedValue({} as RegistrationResponseJSON)
    const futureDelay = new Date(Date.now() + 60_000).toISOString()
    mockFetchByPath({
      '/recovery/requests/start': { ticket_id: 't1', challenge: { publicKey: {} } },
      '/recovery/requests/finish': { ...requestBase, status: 'delay', delay_ends_at: futureDelay },
    })

    const router = testRouter()
    router.push('/recover-identity')
    await router.isReady()
    const wrapper = mount(RecoverIdentity, { global: { plugins: [router] } })
    await startRecovery(wrapper)

    expect(wrapper.text()).not.toContain('Finish recovery')
  })
})
