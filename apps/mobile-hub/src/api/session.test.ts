import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'

const { FakeUnauthorized, resume, resumeWithKey } = vi.hoisted(() => ({
  FakeUnauthorized: class extends Error {},
  resume: vi.fn(),
  resumeWithKey: vi.fn(),
}))

vi.mock('@avalon-initiative/protocol-sdk', () => ({
  UnauthorizedError: FakeUnauthorized,
  AvalonClient: class {
    resumeAccountSession = resume
    resumeAccountSessionWithSigningKey = resumeWithKey
  },
}))

import { configureSessionStorage, type KeyValueStore } from './sessionStorage'
import { storeSigningKeySeed } from './signingKeyStorage'
import { useSessionStore } from './session'

function memoryStore(initial: Record<string, string> = {}): KeyValueStore & { data: Record<string, string> } {
  const data = { ...initial }
  return {
    data,
    async getItem(key) {
      return data[key] ?? null
    },
    async setItem(key, value) {
      data[key] = value
    },
    async removeItem(key) {
      delete data[key]
    },
  }
}

const fakeSession = { token: () => 'tok-1', identity: () => ({ id: 'id-1' }) }

beforeEach(() => {
  setActivePinia(createPinia())
  resume.mockReset()
  resumeWithKey.mockReset()
  localStorage.clear()
})

describe('session store', () => {
  it('stays signed out and becomes ready when nothing is stored', async () => {
    configureSessionStorage(memoryStore())
    const store = useSessionStore()
    await store.initialize()
    expect(store.ready).toBe(true)
    expect(store.isAuthenticated()).toBe(false)
    expect(resume).not.toHaveBeenCalled()
  })

  it('resumes a stored token through the configured store', async () => {
    configureSessionStorage(memoryStore({ 'avalon:session:token': 'tok-1', 'avalon:session:identityId': 'id-1' }))
    resume.mockResolvedValue(fakeSession)
    const store = useSessionStore()
    await store.initialize()
    expect(resume).toHaveBeenCalledWith('tok-1')
    expect(store.isAuthenticated()).toBe(true)
  })

  it('resumes with the signing key when this device holds one', async () => {
    configureSessionStorage(memoryStore({ 'avalon:session:token': 'tok-1', 'avalon:session:identityId': 'id-1' }))
    storeSigningKeySeed('id-1', new Uint8Array([1, 2, 3]))
    resumeWithKey.mockResolvedValue(fakeSession)
    const store = useSessionStore()
    await store.initialize()
    expect(resumeWithKey).toHaveBeenCalledWith('tok-1', new Uint8Array([1, 2, 3]))
    expect(resume).not.toHaveBeenCalled()
  })

  it('clears stored credentials only when the server rejects the token', async () => {
    const store1 = memoryStore({ 'avalon:session:token': 'tok-1' })
    configureSessionStorage(store1)
    resume.mockRejectedValue(new FakeUnauthorized())
    await useSessionStore().initialize()
    expect(store1.data['avalon:session:token']).toBeUndefined()

    setActivePinia(createPinia())
    const store2 = memoryStore({ 'avalon:session:token': 'tok-2' })
    configureSessionStorage(store2)
    resume.mockRejectedValue(new Error('network down'))
    const s = useSessionStore()
    await s.initialize()
    expect(store2.data['avalon:session:token']).toBe('tok-2')
    expect(s.isAuthenticated()).toBe(false)
  })

  it('persists credentials on setSession and clears them (and the key) on logout', async () => {
    const store = memoryStore()
    configureSessionStorage(store)
    storeSigningKeySeed('id-1', new Uint8Array([9]))
    const s = useSessionStore()
    await s.setSession(fakeSession as never)
    expect(store.data).toMatchObject({ 'avalon:session:token': 'tok-1', 'avalon:session:identityId': 'id-1' })

    await s.logout()
    expect(store.data).toEqual({})
    expect(localStorage.getItem('avalon:signingKey:id-1')).toBeNull()
    expect(s.isAuthenticated()).toBe(false)
  })
})
