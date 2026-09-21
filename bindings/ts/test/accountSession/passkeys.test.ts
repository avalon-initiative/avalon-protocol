import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { RegistrationResponseJSON } from '@simplewebauthn/browser'

const runRegistrationCeremonyMock = vi.fn<(options: unknown) => Promise<RegistrationResponseJSON>>()

// Mocking the resolved submodule, rather than this package's own barrel,
// is what actually intercepts the internal call passkeys.ts makes.
vi.mock('../../src/crypto/webauthn.js', () => ({
  runRegistrationCeremony: (options: unknown) => runRegistrationCeremonyMock(options),
}))

import { AccountSession } from '../../src/accountSession/core.js'
import '../../src/accountSession/passkeys.js'

function testProfile(identityId: string) {
  return {
    identityId,
    displayName: 'test',
    avatarUrl: null,
    bio: null,
    favoriteGenres: [],
    pronouns: null,
    bannerUrl: null,
    status: null,
    links: [],
    timezone: null,
    themeColor: null,
    location: null,
    mainGuild: null,
    effectiveMainGuild: null,
    discoverable: false,
    presenceVisibility: 'public',
  }
}

function testSession(signing?: { secretKey: Uint8Array; publicKey: Uint8Array; signingKeyId: string }): AccountSession {
  const identity = { id: crypto.randomUUID(), createdAt: new Date().toISOString() }
  return new AccountSession({
    identity,
    profile: testProfile(identity.id),
    serverUrl: 'http://127.0.0.1:1',
    token: 'test-token',
    signing,
  })
}

function mockFetchOnce(body: unknown) {
  const text = body === undefined ? '' : JSON.stringify(body)
  vi.stubGlobal(
    'fetch',
    vi.fn().mockResolvedValue({ ok: true, status: 200, json: () => Promise.resolve(body), text: () => Promise.resolve(text) }),
  )
}

beforeEach(() => {
  runRegistrationCeremonyMock.mockReset()
})

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('AccountSession.addPasskey', () => {
  it('starts the ceremony, drives it in the browser, then finishes it with the resulting credential and label', async () => {
    const fakeCredential = { id: 'cred-1' } as unknown as RegistrationResponseJSON
    runRegistrationCeremonyMock.mockResolvedValue(fakeCredential)
    const session = testSession()

    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce({
        ok: true,
        status: 200,
        json: () => Promise.resolve({ ticket_id: 'ticket-1', challenge: { publicKey: { fake: true } } }),
        text: () => Promise.resolve(JSON.stringify({ ticket_id: 'ticket-1', challenge: { publicKey: { fake: true } } })),
      })
      .mockResolvedValueOnce({
        ok: true,
        status: 200,
        json: () => Promise.resolve({ id: 'passkey-1', label: 'my phone', added_at: 'now' }),
        text: () => Promise.resolve(JSON.stringify({ id: 'passkey-1', label: 'my phone', added_at: 'now' })),
      })
    vi.stubGlobal('fetch', fetchMock)

    const result = await session.addPasskey('my phone')

    expect(result).toEqual({ id: 'passkey-1', label: 'my phone', addedAt: 'now' })
    expect(runRegistrationCeremonyMock).toHaveBeenCalledWith({ fake: true })

    const [startUrl] = fetchMock.mock.calls[0]
    expect(startUrl).toContain('/me/passkeys/register/start')

    const [finishUrl, finishOptions] = fetchMock.mock.calls[1]
    expect(finishUrl).toContain('/me/passkeys/register/finish')
    const sentBody = JSON.parse(finishOptions.body)
    expect(sentBody).toEqual({ ticket_id: 'ticket-1', webauthn_credential: fakeCredential, label: 'my phone' })
  })
})

describe('AccountSession.listPasskeys', () => {
  it('returns the server list with camelCase fields', async () => {
    mockFetchOnce([{ id: 'p1', label: null, added_at: 'now' }])
    expect(await testSession().listPasskeys()).toEqual([{ id: 'p1', label: null, addedAt: 'now' }])
  })

  it('passes through a non-array body instead of crashing, so callers can detect it themselves', async () => {
    mockFetchOnce(null)
    expect(await testSession().listPasskeys()).toBeNull()
  })
})

describe('AccountSession.renamePasskey', () => {
  it('sends the new label to the right passkey', async () => {
    mockFetchOnce({ id: 'p1', label: 'renamed', added_at: 'now' })
    const result = await testSession().renamePasskey('p1', 'renamed')
    expect(result.label).toBe('renamed')

    const [url, options] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(url).toContain('/me/passkeys/p1')
    expect(JSON.parse(options.body)).toEqual({ label: 'renamed' })
  })
})

describe('AccountSession.revokePasskey', () => {
  it('always signs passkey.revoke_last, even for a device with no local key', async () => {
    mockFetchOnce(undefined)
    const session = testSession()
    await session.revokePasskey('p1')
    const [url, options] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0]
    expect(url).toContain('/me/passkeys/p1/revoke')
    expect(JSON.parse(options.body)).toEqual({ signing_key_id: null, signature: null })
  })
})
