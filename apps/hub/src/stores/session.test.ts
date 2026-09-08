import { createPinia, setActivePinia } from 'pinia'
import { beforeEach, describe, expect, it } from 'vitest'
import { useSessionStore } from './session'

beforeEach(() => {
  localStorage.clear()
  setActivePinia(createPinia())
})

describe('session store', () => {
  it('starts unauthenticated with no stored token', () => {
    const session = useSessionStore()
    expect(session.isAuthenticated()).toBe(false)
    expect(session.token).toBeNull()
  })

  it('login persists the token and marks the session authenticated', () => {
    const session = useSessionStore()
    session.login('a-token')
    expect(session.isAuthenticated()).toBe(true)
    expect(localStorage.getItem('avalon:session:token')).toBe('a-token')
  })

  it('logout clears both the store and localStorage', () => {
    const session = useSessionStore()
    session.login('a-token')
    session.logout()
    expect(session.isAuthenticated()).toBe(false)
    expect(localStorage.getItem('avalon:session:token')).toBeNull()
  })

  it('picks up a token already in localStorage on creation', () => {
    localStorage.setItem('avalon:session:token', 'existing-token')
    const session = useSessionStore()
    expect(session.isAuthenticated()).toBe(true)
    expect(session.token).toBe('existing-token')
  })
})
