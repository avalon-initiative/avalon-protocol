// Session token only — identity/profile data is always re-read from
// GET /me, never cached here. Persisted through
// the pluggable `./storage` adapter (see its own module doc comment):
// `localStorage` by default (hub's original #55/#99/#122 stopgap,
// unchanged), or a platform secure-storage adapter when one has been
// configured (mobile-hub) — this store never touches `localStorage`
// directly so it doesn't have to know which one it's running against.
//
// Storage is inherently async once it isn't guaranteed to be `localStorage`
// (Tauri secure storage goes through an IPC round trip), so this store's
// initial hydration is an explicit `initialize()` action rather than a
// value read synchronously into `ref()` at store-creation time — every
// app's `main.ts` awaits it once, before mounting, so every other call
// site (route guards included) still sees already-hydrated state exactly
// as before this became pluggable.
//
// `reconnect` is a narrow exception to the invariant above
// above: not profile data (never displayed, never treated as authoritative
// for anything but minting), just the two identifiers
// `crypto/continuation.ts::mintContinuationToken` needs to prove "the
// holder of this identity's signing key wants to act now" against a
// *different* node than the one that issued `token` — without them, a
// continuation token could only ever be minted right after a fresh
// GET /me call, which defeats the point when the node that would answer
// that call is exactly the one that's unreachable.
import { defineStore } from 'pinia'
import { ref } from 'vue'
import { getSessionStorage } from './storage'

const STORAGE_KEY = 'avalon:session:token'
const IDENTITY_ID_STORAGE_KEY = 'avalon:session:identityId'
const SIGNING_KEY_ID_STORAGE_KEY = 'avalon:session:signingKeyId'

export const useSessionStore = defineStore('session', () => {
  const token = ref<string | null>(null)
  const identityId = ref<string | null>(null)
  const signingKeyId = ref<string | null>(null)
  // False until `initialize()` resolves — a caller that genuinely needs to
  // distinguish "not logged in" from "haven't hydrated yet" (none exist
  // today; every app awaits `initialize()` before mounting) can check this.
  const ready = ref(false)

  /**
   * Hydrates the three refs above from storage. Every app's `main.ts` calls
   * and awaits this exactly once, before mounting, so route guards and
   * every other call site see final state on their very first read —
   * same behavior the old synchronous `ref(localStorage.getItem(...))`
   * gave hub, just no longer assuming storage is synchronous.
   */
  async function initialize() {
    const store = getSessionStorage()
    const [storedToken, storedIdentityId, storedSigningKeyId] = await Promise.all([
      store.getItem(STORAGE_KEY),
      store.getItem(IDENTITY_ID_STORAGE_KEY),
      store.getItem(SIGNING_KEY_ID_STORAGE_KEY),
    ])
    token.value = storedToken
    identityId.value = storedIdentityId
    signingKeyId.value = storedSigningKeyId
    ready.value = true
  }

  /**
   * `reconnectCredentials`, when present, are the two fields above —
   * `identityId` is known at every login call site already (the user
   * typed it, or just created/recovered it); `signingKeyId` is optional
   * because it requires this device to actually hold a local signing key
   * (`findMySigningKeyId`) — a caller with none (e.g. `#122`'s "still
   * possible to have a session with no local signing key yet" case) still
   * logs in normally, just without reconnect-across-nodes support until
   * one exists.
   */
  // `newIdentityId`/`newSigningKeyId` default to `null` — every real login
  // call site (Login.vue, CreateIdentity.vue, RecoverIdentity.vue) passes
  // them explicitly; the default only exists so call sites/tests that
  // don't care about cross-node reconnect (most of this test suite) can
  // keep calling `login('a-token')` unchanged, same as before this ticket.
  //
  // Refs update synchronously, before the first `await` below, so a caller
  // that doesn't await this (existing test/call-site style) still observes
  // the new state immediately — only the storage write itself is async.
  async function login(
    newToken: string,
    newIdentityId: string | null = null,
    newSigningKeyId: string | null = null,
  ) {
    token.value = newToken
    identityId.value = newIdentityId
    signingKeyId.value = newSigningKeyId
    const store = getSessionStorage()
    await Promise.all([
      store.setItem(STORAGE_KEY, newToken),
      newIdentityId ? store.setItem(IDENTITY_ID_STORAGE_KEY, newIdentityId) : store.removeItem(IDENTITY_ID_STORAGE_KEY),
      newSigningKeyId
        ? store.setItem(SIGNING_KEY_ID_STORAGE_KEY, newSigningKeyId)
        : store.removeItem(SIGNING_KEY_ID_STORAGE_KEY),
    ])
  }

  /**
   * Issue #525: updates just `signingKeyId`, for the two mid-session
   * moments a device can go from having no local signing key to having
   * one without a fresh login — `Profile.vue`'s mnemonic-recovery
   * (`onRecoverSigningKey`) and device-grant-approval (`pollMyGrant`)
   * paths. `token`/`identityId` are already correct in both cases; only
   * this needs to change.
   */
  async function setSigningKeyId(newSigningKeyId: string | null) {
    signingKeyId.value = newSigningKeyId
    const store = getSessionStorage()
    if (newSigningKeyId) {
      await store.setItem(SIGNING_KEY_ID_STORAGE_KEY, newSigningKeyId)
    } else {
      await store.removeItem(SIGNING_KEY_ID_STORAGE_KEY)
    }
  }

  async function logout() {
    token.value = null
    identityId.value = null
    signingKeyId.value = null
    const store = getSessionStorage()
    await Promise.all([
      store.removeItem(STORAGE_KEY),
      store.removeItem(IDENTITY_ID_STORAGE_KEY),
      store.removeItem(SIGNING_KEY_ID_STORAGE_KEY),
    ])
  }

  const isAuthenticated = () => token.value !== null

  return { token, identityId, signingKeyId, ready, initialize, login, setSigningKeyId, logout, isAuthenticated }
})
