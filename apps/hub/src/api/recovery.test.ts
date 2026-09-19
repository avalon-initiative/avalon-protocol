// Social recovery (issue #201) — exercises the orchestration in
// api/recovery.ts, same mocking approach passkeys.test.ts already
// established: `fetch` is stubbed for the server round trip, and
// `runRegistrationCeremony` is mocked since there's no real authenticator
// in a unit test.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { RegistrationResponseJSON } from '@simplewebauthn/browser'

const runRegistrationCeremonyMock = vi.fn<
  (options: unknown) => Promise<RegistrationResponseJSON>
>()

// api/recovery.ts calls runRegistrationCeremony indirectly (via
// @avalon/api-client's own recovery.ts, which imports it from its
// sibling ./crypto/webauthn) — mocking that resolved file directly, rather
// than the @avalon/api-client package entrypoint, is what actually
// intercepts the internal call; a mock of the barrel export wouldn't
// reach a call made from *inside* that same package.
vi.mock('@avalon/api-client/src/crypto/webauthn', () => ({
  runRegistrationCeremony: (options: unknown) => runRegistrationCeremonyMock(options),
}))

import {
  approveRecoveryRequest,
  cancelRecoveryRequest,
  getGuardianRequests,
  getGuardians,
  getMyRecoveryStatus,
  setGuardians,
  startRecovery,
} from './recovery'

function mockFetchOnce(body: unknown) {
  const text = body === undefined ? '' : JSON.stringify(body)
  vi.stubGlobal(
    'fetch',
    vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: () => Promise.resolve(body),
      text: () => Promise.resolve(text),
    }),
  )
}

beforeEach(() => {
  runRegistrationCeremonyMock.mockReset()
})

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('getGuardians / setGuardians', () => {
  it('fetches the caller own guardian configuration', async () => {
    mockFetchOnce({ guardian_ids: ['g1'], threshold: 1, updated_at: 'now' })
    const result = await getGuardians('token')
    expect(result.guardian_ids).toEqual(['g1'])

    const [url] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(url).toContain('/me/recovery/guardians')
  })

  it('sends the full guardian set and threshold on update', async () => {
    mockFetchOnce({ guardian_ids: ['g1', 'g2'], threshold: 2, updated_at: 'now' })
    await setGuardians('token', ['g1', 'g2'], 2)

    const [url, options] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(url).toContain('/me/recovery/guardians')
    expect(options.method).toBe('PUT')
    expect(JSON.parse(options.body)).toEqual({ guardian_ids: ['g1', 'g2'], threshold: 2 })
  })
})

describe('getMyRecoveryStatus', () => {
  it('returns null when nothing is in progress', async () => {
    mockFetchOnce(null)
    expect(await getMyRecoveryStatus('token')).toBeNull()
  })

  it('returns the active request when one exists', async () => {
    const active = {
      id: 'r1',
      identity_id: 'me',
      status: 'delay',
      threshold: 2,
      approvals_count: 2,
      requested_at: 'now',
      delay_ends_at: 'later',
    }
    mockFetchOnce(active)
    expect(await getMyRecoveryStatus('token')).toEqual(active)
  })
})

describe('getGuardianRequests', () => {
  it('returns every pending request the caller can approve', async () => {
    const summaries = [
      {
        request: {
          id: 'r1',
          identity_id: 'friend-1',
          status: 'pending_approvals',
          threshold: 2,
          approvals_count: 1,
          requested_at: 'now',
          delay_ends_at: null,
        },
        already_approved: false,
      },
    ]
    mockFetchOnce(summaries)
    expect(await getGuardianRequests('token')).toEqual(summaries)
  })
})

describe('approveRecoveryRequest / cancelRecoveryRequest', () => {
  it('approves against the right request id', async () => {
    mockFetchOnce({
      id: 'r1',
      identity_id: 'friend-1',
      status: 'delay',
      threshold: 2,
      approvals_count: 2,
      requested_at: 'now',
      delay_ends_at: 'later',
    })
    await approveRecoveryRequest('token', 'r1')
    const [url, options] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(url).toContain('/recovery/requests/r1/approve')
    expect(options.method).toBe('POST')
  })

  it('cancels with an optional reason', async () => {
    mockFetchOnce({
      id: 'r1',
      identity_id: 'friend-1',
      status: 'cancelled',
      threshold: 2,
      approvals_count: 1,
      requested_at: 'now',
      delay_ends_at: null,
    })
    await cancelRecoveryRequest('token', 'r1', 'not me')
    const [url, options] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(url).toContain('/recovery/requests/r1/cancel')
    expect(JSON.parse(options.body)).toEqual({ reason: 'not me' })
  })
})

describe('startRecovery', () => {
  it('starts the ceremony unauthenticated, drives it in the browser, then finishes it', async () => {
    const fakeCredential = { id: 'cred-1' } as unknown as RegistrationResponseJSON
    runRegistrationCeremonyMock.mockResolvedValue(fakeCredential)

    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce({
        ok: true,
        status: 200,
        json: () =>
          Promise.resolve({ ticket_id: 'ticket-1', challenge: { publicKey: { fake: true } } }),
        text: () =>
          Promise.resolve(
            JSON.stringify({ ticket_id: 'ticket-1', challenge: { publicKey: { fake: true } } }),
          ),
      })
      .mockResolvedValueOnce({
        ok: true,
        status: 200,
        json: () =>
          Promise.resolve({
            id: 'req-1',
            identity_id: 'identity-1',
            status: 'pending_approvals',
            threshold: 2,
            approvals_count: 0,
            requested_at: 'now',
            delay_ends_at: null,
          }),
        text: () =>
          Promise.resolve(
            JSON.stringify({
              id: 'req-1',
              identity_id: 'identity-1',
              status: 'pending_approvals',
              threshold: 2,
              approvals_count: 0,
              requested_at: 'now',
              delay_ends_at: null,
            }),
          ),
      })
    vi.stubGlobal('fetch', fetchMock)

    const result = await startRecovery('identity-1', 'new phone')

    expect(result.status).toBe('pending_approvals')
    expect(runRegistrationCeremonyMock).toHaveBeenCalledWith({ fake: true })

    const [startUrl, startOptions] = fetchMock.mock.calls[0]
    expect(startUrl).toContain('/recovery/requests/start')
    // No bearer token — this is the one deliberately unauthenticated path.
    expect(startOptions.headers.Authorization).toBeUndefined()
    expect(JSON.parse(startOptions.body)).toEqual({
      identity_id: 'identity-1',
      device_label: 'new phone',
    })

    const [finishUrl, finishOptions] = fetchMock.mock.calls[1]
    expect(finishUrl).toContain('/recovery/requests/finish')
    expect(finishOptions.headers.Authorization).toBeUndefined()
    expect(JSON.parse(finishOptions.body)).toEqual({
      ticket_id: 'ticket-1',
      webauthn_credential: fakeCredential,
    })
  })
})
