// Multi-passkey registration (issue #200) — exercises the orchestration in
// api/passkeys.ts, mocking both `fetch` (the server round trip) and the
// WebAuthn ceremony itself (no real authenticator in a unit test), same
// approach identity.test.ts-style coverage would take for createIdentity()
// if it existed; this module has no prior test file to extend.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { RegistrationResponseJSON } from '@simplewebauthn/browser'

const runRegistrationCeremonyMock = vi.fn<
  (options: unknown) => Promise<RegistrationResponseJSON>
>()

// See recovery.test.ts's own comment on why this mocks the resolved
// @avalon/api-client submodule rather than the package entrypoint.
vi.mock('@avalon/api-client/src/crypto/webauthn', () => ({
  runRegistrationCeremony: (options: unknown) => runRegistrationCeremonyMock(options),
}))

import { addPasskey, listPasskeys, renamePasskey, revokePasskey } from './passkeys'

function mockFetchOnce(body: unknown) {
  // Mirrors client.ts's own convention: a `Result<(), AppError>` handler
  // (e.g. revoke) serializes as 200 with an empty body, not a JSON `null` —
  // `body === undefined` reproduces that exactly, same as
  // deviceGrants.test.ts's precedent doesn't need since none of its calls
  // hit a `()`-returning endpoint.
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

describe('addPasskey', () => {
  it('starts the ceremony, drives it in the browser, then finishes it with the resulting credential and label', async () => {
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
        json: () => Promise.resolve({ id: 'passkey-1', label: 'my phone', added_at: 'now' }),
        text: () =>
          Promise.resolve(
            JSON.stringify({ id: 'passkey-1', label: 'my phone', added_at: 'now' }),
          ),
      })
    vi.stubGlobal('fetch', fetchMock)

    const result = await addPasskey('token', 'my phone')

    expect(result).toEqual({ id: 'passkey-1', label: 'my phone', added_at: 'now' })
    expect(runRegistrationCeremonyMock).toHaveBeenCalledWith({ fake: true })

    const [startUrl] = fetchMock.mock.calls[0]
    expect(startUrl).toContain('/me/passkeys/register/start')

    const [finishUrl, finishOptions] = fetchMock.mock.calls[1]
    expect(finishUrl).toContain('/me/passkeys/register/finish')
    const sentBody = JSON.parse(finishOptions.body)
    expect(sentBody).toEqual({
      ticket_id: 'ticket-1',
      webauthn_credential: fakeCredential,
      label: 'my phone',
    })
  })
})

describe('listPasskeys', () => {
  it('returns whatever the server sends back', async () => {
    const passkeys = [{ id: 'p1', label: null, added_at: 'now' }]
    mockFetchOnce(passkeys)
    expect(await listPasskeys('token')).toEqual(passkeys)
  })
})

describe('renamePasskey', () => {
  it('sends the new label to the right passkey', async () => {
    mockFetchOnce({ id: 'p1', label: 'renamed', added_at: 'now' })
    const result = await renamePasskey('token', 'p1', 'renamed')
    expect(result.label).toBe('renamed')

    const [url, options] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(url).toContain('/me/passkeys/p1')
    expect(JSON.parse(options.body)).toEqual({ label: 'renamed' })
  })
})

describe('revokePasskey', () => {
  it('omits ?confirm= when not confirming', async () => {
    mockFetchOnce(undefined)
    await revokePasskey('token', 'p1')
    const [url] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(url).toContain('/me/passkeys/p1/revoke')
    expect(url).not.toContain('confirm')
  })

  it('appends ?confirm=true when confirming', async () => {
    mockFetchOnce(undefined)
    await revokePasskey('token', 'p1', true)
    const [url] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(url).toContain('/me/passkeys/p1/revoke?confirm=true')
  })
})
