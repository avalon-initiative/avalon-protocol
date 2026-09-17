// Session token only — identity/profile data is always re-read from
// GET /me, never cached here (issue #55's invariant). Persisted to
// localStorage as a milestone-1 stopgap, same status as the signing key in
// crypto/signingKey.ts — see #99/#122.
//
// `reconnect` (issue #525) is a narrow exception to that invariant: not
// profile data (never displayed, never treated as authoritative for
// anything but minting), just the two identifiers
// `crypto/continuation.ts::mintContinuationToken` needs to prove "the
// holder of this identity's signing key wants to act now" against a
// *different* node than the one that issued `token` — without them, a
// continuation token could only ever be minted right after a fresh
// GET /me call, which defeats the point when the node that would answer
// that call is exactly the one that's unreachable.
import { defineStore } from 'pinia'
import { ref } from 'vue'

const STORAGE_KEY = 'avalon:session:token'
const IDENTITY_ID_STORAGE_KEY = 'avalon:session:identityId'
const SIGNING_KEY_ID_STORAGE_KEY = 'avalon:session:signingKeyId'

export const useSessionStore = defineStore('session', () => {
  const token = ref<string | null>(localStorage.getItem(STORAGE_KEY))
  const identityId = ref<string | null>(localStorage.getItem(IDENTITY_ID_STORAGE_KEY))
  const signingKeyId = ref<string | null>(localStorage.getItem(SIGNING_KEY_ID_STORAGE_KEY))

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
  function login(
    newToken: string,
    newIdentityId: string | null = null,
    newSigningKeyId: string | null = null,
  ) {
    token.value = newToken
    identityId.value = newIdentityId
    signingKeyId.value = newSigningKeyId
    localStorage.setItem(STORAGE_KEY, newToken)
    if (newIdentityId) {
      localStorage.setItem(IDENTITY_ID_STORAGE_KEY, newIdentityId)
    } else {
      localStorage.removeItem(IDENTITY_ID_STORAGE_KEY)
    }
    if (newSigningKeyId) {
      localStorage.setItem(SIGNING_KEY_ID_STORAGE_KEY, newSigningKeyId)
    } else {
      localStorage.removeItem(SIGNING_KEY_ID_STORAGE_KEY)
    }
  }

  /**
   * Issue #525: updates just `signingKeyId`, for the two mid-session
   * moments a device can go from having no local signing key to having
   * one without a fresh login — `Profile.vue`'s mnemonic-recovery
   * (`onRecoverSigningKey`) and device-grant-approval (`pollMyGrant`)
   * paths. `token`/`identityId` are already correct in both cases; only
   * this needs to change.
   */
  function setSigningKeyId(newSigningKeyId: string | null) {
    signingKeyId.value = newSigningKeyId
    if (newSigningKeyId) {
      localStorage.setItem(SIGNING_KEY_ID_STORAGE_KEY, newSigningKeyId)
    } else {
      localStorage.removeItem(SIGNING_KEY_ID_STORAGE_KEY)
    }
  }

  function logout() {
    token.value = null
    identityId.value = null
    signingKeyId.value = null
    localStorage.removeItem(STORAGE_KEY)
    localStorage.removeItem(IDENTITY_ID_STORAGE_KEY)
    localStorage.removeItem(SIGNING_KEY_ID_STORAGE_KEY)
  }

  const isAuthenticated = () => token.value !== null

  return { token, identityId, signingKeyId, login, setSigningKeyId, logout, isAuthenticated }
})
