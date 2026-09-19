import { createPinia, setActivePinia } from 'pinia'
import { beforeEach, describe, expect, it } from 'vitest'
import { useSessionStore } from './session'

beforeEach(() => {
  localStorage.clear()
  setActivePinia(createPinia())
})

describe('session store', () => {
  it('starts unauthenticated with no stored token', async () => {
    const session = useSessionStore()
    await session.initialize()
    expect(session.isAuthenticated()).toBe(false)
    expect(session.token).toBeNull()
  })

  it('login persists the token and marks the session authenticated', async () => {
    const session = useSessionStore()
    await session.login('a-token')
    expect(session.isAuthenticated()).toBe(true)
    expect(localStorage.getItem('avalon:session:token')).toBe('a-token')
  })

  it('login updates refs synchronously, before the storage write resolves', () => {
    const session = useSessionStore()
    const promise = session.login('a-token')
    // Refs are set before the first await inside login() — a caller that
    // doesn't await (most call sites, most of this suite) still sees
    // correct state immediately.
    expect(session.isAuthenticated()).toBe(true)
    return promise
  })

  it('logout clears both the store and localStorage', async () => {
    const session = useSessionStore()
    await session.login('a-token')
    await session.logout()
    expect(session.isAuthenticated()).toBe(false)
    expect(localStorage.getItem('avalon:session:token')).toBeNull()
  })

  it('picks up a token already in localStorage on initialize', async () => {
    localStorage.setItem('avalon:session:token', 'existing-token')
    const session = useSessionStore()
    await session.initialize()
    expect(session.isAuthenticated()).toBe(true)
    expect(session.token).toBe('existing-token')
  })

  // Issue #525: identityId/signingKeyId are the reconnect-across-nodes
  // material — see this store's own module doc comment for why caching
  // them doesn't conflict with #55's "profile data always re-read from
  // GET /me" invariant.
  describe('reconnect credentials (issue #525)', () => {
    it('login persists identityId and signingKeyId alongside the token', async () => {
      const session = useSessionStore()
      await session.login('a-token', 'identity-1', 'key-1')
      expect(session.identityId).toBe('identity-1')
      expect(session.signingKeyId).toBe('key-1')
      expect(localStorage.getItem('avalon:session:identityId')).toBe('identity-1')
      expect(localStorage.getItem('avalon:session:signingKeyId')).toBe('key-1')
    })

    it('login defaults identityId/signingKeyId to null when omitted', async () => {
      const session = useSessionStore()
      await session.login('a-token')
      expect(session.identityId).toBeNull()
      expect(session.signingKeyId).toBeNull()
    })

    it('login with a null signingKeyId (no local signing key yet) stores none', async () => {
      const session = useSessionStore()
      await session.login('a-token', 'identity-1', null)
      expect(session.signingKeyId).toBeNull()
      expect(localStorage.getItem('avalon:session:signingKeyId')).toBeNull()
    })

    it('setSigningKeyId updates just that field, independent of token/identityId', async () => {
      const session = useSessionStore()
      await session.login('a-token', 'identity-1', null)
      await session.setSigningKeyId('key-later')
      expect(session.token).toBe('a-token')
      expect(session.identityId).toBe('identity-1')
      expect(session.signingKeyId).toBe('key-later')
      expect(localStorage.getItem('avalon:session:signingKeyId')).toBe('key-later')
    })

    it('logout clears identityId and signingKeyId too', async () => {
      const session = useSessionStore()
      await session.login('a-token', 'identity-1', 'key-1')
      await session.logout()
      expect(session.identityId).toBeNull()
      expect(session.signingKeyId).toBeNull()
      expect(localStorage.getItem('avalon:session:identityId')).toBeNull()
      expect(localStorage.getItem('avalon:session:signingKeyId')).toBeNull()
    })

    it('picks up identityId/signingKeyId already in localStorage on initialize', async () => {
      localStorage.setItem('avalon:session:token', 'existing-token')
      localStorage.setItem('avalon:session:identityId', 'existing-identity')
      localStorage.setItem('avalon:session:signingKeyId', 'existing-key')
      const session = useSessionStore()
      await session.initialize()
      expect(session.identityId).toBe('existing-identity')
      expect(session.signingKeyId).toBe('existing-key')
    })
  })
})
