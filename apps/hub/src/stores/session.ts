// Session token only — identity/profile data is always re-read from
// GET /me, never cached here (issue #55's invariant). Persisted to
// localStorage as a milestone-1 stopgap, same status as the signing key in
// crypto/signingKey.ts — see #99/#122.
import { defineStore } from 'pinia'
import { ref } from 'vue'

const STORAGE_KEY = 'avalon:session:token'

export const useSessionStore = defineStore('session', () => {
  const token = ref<string | null>(localStorage.getItem(STORAGE_KEY))

  function login(newToken: string) {
    token.value = newToken
    localStorage.setItem(STORAGE_KEY, newToken)
  }

  function logout() {
    token.value = null
    localStorage.removeItem(STORAGE_KEY)
  }

  const isAuthenticated = () => token.value !== null

  return { token, login, logout, isAuthenticated }
})
