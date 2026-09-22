// Cross-node login (epic #623) — free-standing functions, not
// AccountSession methods: these target an arbitrary destination node
// (`baseUrl`), not this session's own configured server, and
// `submitCrossNodeLoginGrant` needs no session at all for the identity
// being logged in — only its raw signing-key material, the same shape
// apps/hub's own CrossNodeLogin.vue call site already uses today (loading
// identityId/signingKeyId/secretKey straight out of its own storage
// adapter rather than holding a full AccountSession against baseUrl). See
// crates/server/src/cross_node_login.rs and
// packages/api-client/src/client.ts's own three cross-node functions for
// the port this mirrors.
import { request } from './http.js'
import { mintCrossNodeLoginGrant, type CrossNodeLoginGrant } from './crypto/crossNodeLogin.js'
import type { components } from './generated.js'

export interface CrossNodeLoginLookup {
  status: 'pending' | 'denied' | 'expired' | 'approved'
  requestingContext: string
  expiresIn: number
  // Issue #649, implementing #642's decided requirement: whether the
  // requesting node resolves to a real, registered integrator (or a known
  // network anchor). Never a hard gate — an unverified requester still
  // gets a prompt, just a clearly flagged one.
  integratorVerified: boolean
  // Only ever present when `integratorVerified` is `true`.
  displayName?: string
}
interface CrossNodeLoginLookupWire {
  status: 'pending' | 'denied' | 'expired' | 'approved'
  requesting_context: string
  expires_in: number
  integrator_verified: boolean
  display_name?: string
}

/** `GET {baseUrl}/auth/cross-node/lookup?user_code=` — what an approval
 * screen calls before rendering a prompt at all, per #642's decided
 * phishing-context requirement: real context shown before a human can act,
 * never just a bare "approve?". */
export async function lookupCrossNodeLogin(baseUrl: string, userCode: string): Promise<CrossNodeLoginLookup> {
  const w = await request<CrossNodeLoginLookupWire>(
    baseUrl,
    `/auth/cross-node/lookup?user_code=${encodeURIComponent(userCode)}`,
  )
  return {
    status: w.status,
    requestingContext: w.requesting_context,
    expiresIn: w.expires_in,
    integratorVerified: w.integrator_verified,
    displayName: w.display_name,
  }
}

/** `POST {baseUrl}/auth/cross-node/deny` — deliberately unauthenticated
 * server-side: the approver's session, if any, routinely lives on a
 * different node than the one this request was started on. */
export async function denyCrossNodeLogin(baseUrl: string, userCode: string): Promise<void> {
  await request(baseUrl, '/auth/cross-node/deny', { method: 'POST', body: { user_code: userCode } })
}

/** `POST {baseUrl}/auth/cross-node/submit` — mints a `CrossNodeLoginGrant`
 * locally with the identity's own event-signing key
 * (`crypto/crossNodeLogin.ts::mintCrossNodeLoginGrant`), then submits it
 * (with `userCode` attached, when known) to resolve the pending request an
 * unfamiliar device/console started. Takes raw identity/key material
 * rather than an `AccountSession`, matching how apps/hub's own
 * `CrossNodeLogin.vue` call site already needs to shape this call: the
 * approving browser's ambient session (if any) has nothing to do with
 * `destinationBaseUrl`. Returns the destination node's bearer token when
 * this submission itself completes login there, `undefined` when it only
 * records approval for the requesting device to poll for separately. */
export async function submitCrossNodeLoginGrant(
  baseUrl: string,
  identityId: string,
  signingKeyId: string,
  signingKeySecret: Uint8Array,
  destinationBaseUrl: string,
  requestingContext: string,
  userCode?: string,
): Promise<string | undefined> {
  const grant: CrossNodeLoginGrant = mintCrossNodeLoginGrant(
    identityId,
    signingKeyId,
    signingKeySecret,
    destinationBaseUrl,
    requestingContext,
  )
  const w = await request<{ token?: string }>(baseUrl, '/auth/cross-node/submit', {
    method: 'POST',
    body: { user_code: userCode, grant },
  })
  return w.token
}

export interface CrossNodeLoginStart {
  requestCode: string
  userCode: string
  requestingContext: string
  expiresIn: number
  pollInterval: number
}
type CrossNodeLoginStartWire = components['schemas']['StartCrossNodeLoginResponse']

/** `POST {baseUrl}/auth/cross-node/start` — called on the *requesting* node
 * (a game/app console with no WebAuthn ceremony surface of its own) to mint
 * the `userCode`/`requestCode` pair: `userCode` is shown to the human for
 * typing/scanning on their own already-logged-in device, `requestCode` is
 * this device's own opaque polling credential — never shown to the human,
 * never sent anywhere but `pollCrossNodeLogin`. Unauthenticated. */
export async function startCrossNodeLogin(baseUrl: string): Promise<CrossNodeLoginStart> {
  const w = await request<CrossNodeLoginStartWire>(baseUrl, '/auth/cross-node/start', { method: 'POST' })
  return {
    requestCode: w.request_code,
    userCode: w.user_code,
    requestingContext: w.requesting_context,
    expiresIn: w.expires_in,
    pollInterval: w.poll_interval,
  }
}

export interface CrossNodeLoginPoll {
  // One of `pending`, `slow_down`, `denied`, `expired`, `approved`.
  status: string
  token?: string
  expiresAt?: string
}
type CrossNodeLoginPollWire = components['schemas']['PollCrossNodeLoginResponse']

/** `POST {baseUrl}/auth/cross-node/poll` — called by the requesting device,
 * bearing `requestCode` (from `startCrossNodeLogin`) as its bearer token —
 * this is an opaque polling credential, not a session token. Never
 * poll faster than `startCrossNodeLogin`'s own `pollInterval`; a `pending`
 * result becomes `slow_down` if this is ignored. `token` is only ever
 * present once, on the single poll that observes `status: 'approved'` —
 * the request is consumed atomically at that point, so a later poll for
 * the same `requestCode` sees `expired`, not a repeat of the same token. */
export async function pollCrossNodeLogin(baseUrl: string, requestCode: string): Promise<CrossNodeLoginPoll> {
  const w = await request<CrossNodeLoginPollWire>(baseUrl, '/auth/cross-node/poll', {
    method: 'POST',
    token: requestCode,
  })
  return { status: w.status, token: w.token ?? undefined, expiresAt: w.expires_at ?? undefined }
}
