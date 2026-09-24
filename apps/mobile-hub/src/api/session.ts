// Session state backed by a real `@avalon-initiative/protocol-sdk` AccountSession.
// The store holds the live session object; views call methods on `session.value`
// directly rather than passing a raw token to free functions.
//
// Credentials (token, identity id) persist through the configurable
// KeyValueStore in ./sessionStorage, which is the OS keychain in the Tauri build.
// The signing-key seed is per-identity and stays in localStorage
// (./signingKeyStorage).
//
// `initialize()` does a real GET /me round trip, since constructing an
// AccountSession requires one. Only an UnauthorizedError clears stored
// credentials; a network or server failure leaves them untouched and the
// session null for this launch, so the next launch can retry.
import { defineStore } from 'pinia'
import { ref, shallowRef } from 'vue'
import { AvalonClient, UnauthorizedError, type AccountSession } from '@avalon-initiative/protocol-sdk'
import { getServerUrl } from './serverUrl'
import { getSessionStorage } from './sessionStorage'
import { clearSigningKeySeed, loadSigningKeySeed } from './signingKeyStorage'

const TOKEN_STORAGE_KEY = 'avalon:session:token'
const IDENTITY_ID_STORAGE_KEY = 'avalon:session:identityId'

export function avalonClient(): AvalonClient {
  return new AvalonClient({ serverUrl: getServerUrl() })
}

export const useSessionStore = defineStore('accountSession', () => {
  const session = shallowRef<AccountSession | null>(null)
  // False until `initialize()` resolves; main.ts awaits it once before mounting,
  // so route guards and every call site see final state.
  const ready = ref(false)

  async function persist(newSession: AccountSession) {
    const store = getSessionStorage()
    await Promise.all([
      store.setItem(TOKEN_STORAGE_KEY, newSession.token()),
      store.setItem(IDENTITY_ID_STORAGE_KEY, newSession.identity().id),
    ])
  }

  async function clearStorage() {
    const store = getSessionStorage()
    await Promise.all([store.removeItem(TOKEN_STORAGE_KEY), store.removeItem(IDENTITY_ID_STORAGE_KEY)])
  }

  async function initialize() {
    const store = getSessionStorage()
    const token = await store.getItem(TOKEN_STORAGE_KEY)
    if (token) {
      const identityId = await store.getItem(IDENTITY_ID_STORAGE_KEY)
      const seed = identityId ? loadSigningKeySeed(identityId) : null
      try {
        session.value = seed
          ? await avalonClient().resumeAccountSessionWithSigningKey(token, seed)
          : await avalonClient().resumeAccountSession(token)
      } catch (e) {
        if (e instanceof UnauthorizedError) {
          await clearStorage()
        }
      }
    }
    ready.value = true
  }

  /** Adopts `newSession` as the active session and persists its credentials. */
  async function setSession(newSession: AccountSession) {
    session.value = newSession
    await persist(newSession)
  }

  async function logout() {
    const identityId = session.value?.identity().id
    session.value = null
    await clearStorage()
    if (identityId) clearSigningKeySeed(identityId)
  }

  const isAuthenticated = () => session.value !== null
  const identityId = () => session.value?.identity().id ?? null
  const signingKeyId = () => session.value?.signingKeyId() ?? null

  return { session, ready, initialize, setSession, logout, isAuthenticated, identityId, signingKeyId }
})
