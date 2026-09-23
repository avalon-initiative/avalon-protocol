// Session state, backed by a real @avalon/sdk AccountSession —
// replaces packages/api-client's own token-only useSessionStore. Unlike
// the old store, this one holds a live AccountSession object, not just a
// bearer token: every other api/*.ts file calls methods on `session.value`
// directly rather than passing a raw token to a free function.
//
// Storage is plain localStorage (Hub only ever used the default adapter —
// packages/api-client's pluggable-storage machinery for mobile-hub's Tauri
// secure storage stays there, mobile-hub isn't migrating in this ticket),
// same two keys `packages/api-client/src/session.ts` already used, so an
// existing viewer's stored session survives this migration unchanged.
//
// `initialize()` now does a real GET /me round trip (constructing an
// AccountSession requires one — there's no way to build one from a bare
// token without validating it) where the old store just trusted
// localStorage synchronously. Only an actual UnauthorizedError clears the
// stored session; a network/transient failure leaves storage untouched and
// `session.value` null for this load, so a later reload can retry cleanly
// rather than logging a viewer out over a flaky connection.
import { defineStore } from 'pinia'
import { ref, shallowRef } from 'vue'
import { AvalonClient, UnauthorizedError, type AccountSession } from '@avalon/sdk'
import { getServerUrl } from './serverUrl'
import { loadSigningKeySeed, clearSigningKeySeed } from './signingKeyStorage'

const TOKEN_STORAGE_KEY = 'avalon:session:token'
const IDENTITY_ID_STORAGE_KEY = 'avalon:session:identityId'

export function avalonClient(): AvalonClient {
  return new AvalonClient({ serverUrl: getServerUrl() })
}

// Pinia id deliberately distinct from packages/api-client/src/session.ts's
// own `defineStore('session', ...)` — Pinia's registry is keyed by this
// string, and colliding ids would make whichever store's `useSessionStore()`
// runs first in a given app "win," silently handing every later caller
// (old or new) that same instance regardless of which module they imported
// from. Not yet renamable back to `'session'`: apps/hub still has
// not-yet-migrated modules (useMyGuilds.ts, friends.ts, etc., #712's later
// batches) that import the OLD store under that id.
export const useSessionStore = defineStore('accountSession', () => {
  const session = shallowRef<AccountSession | null>(null)
  // False until `initialize()` resolves — mirrors the old store's own
  // `ready` flag; every app's `main.ts` awaits `initialize()` once, before
  // mounting, so route guards and every other call site see final state.
  const ready = ref(false)

  function persist(newSession: AccountSession) {
    localStorage.setItem(TOKEN_STORAGE_KEY, newSession.token())
    localStorage.setItem(IDENTITY_ID_STORAGE_KEY, newSession.identity().id)
  }

  function clearStorage() {
    localStorage.removeItem(TOKEN_STORAGE_KEY)
    localStorage.removeItem(IDENTITY_ID_STORAGE_KEY)
  }

  async function initialize() {
    const token = localStorage.getItem(TOKEN_STORAGE_KEY)
    if (token) {
      const identityId = localStorage.getItem(IDENTITY_ID_STORAGE_KEY)
      const seed = identityId ? loadSigningKeySeed(identityId) : null
      try {
        session.value = seed
          ? await avalonClient().resumeAccountSessionWithSigningKey(token, seed)
          : await avalonClient().resumeAccountSession(token)
      } catch (e) {
        if (e instanceof UnauthorizedError) {
          clearStorage()
        }
        // Any other failure (network, server unavailable): leave storage
        // untouched, session stays null for this load — see this file's
        // own header comment.
      }
    }
    ready.value = true
  }

  /** Adopts `newSession` as the active session, persisting its token/
   * identity id. Every login/register/recovery/device-pairing flow calls
   * this once it has a working `AccountSession`. */
  function setSession(newSession: AccountSession) {
    session.value = newSession
    persist(newSession)
  }

  async function logout() {
    const identityId = session.value?.identity().id
    session.value = null
    clearStorage()
    if (identityId) clearSigningKeySeed(identityId)
  }

  const isAuthenticated = () => session.value !== null
  /** The active session's own bearer token — most call sites should use
   * `session.value` directly instead; kept for the few places (the
   * websocket URL, direct fetch fallbacks) that need the raw string. */
  const token = () => session.value?.token() ?? null
  const identityId = () => session.value?.identity().id ?? null
  const signingKeyId = () => session.value?.signingKeyId() ?? null

  return { session, ready, initialize, setSession, logout, isAuthenticated, token, identityId, signingKeyId }
})
