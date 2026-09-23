// Social recovery request-initiation — free-standing
// functions, deliberately not AccountSession methods: the caller has no
// session yet for the identity being recovered, the same reasoning
// AccountSession's own doc comment gives for why `register`/`login` are
// ceremony-driving AvalonClient entry points rather than instance methods.
// See crates/server/src/recovery.rs, and accountSession/recovery.ts for the
// already-logged-in side of this same feature (guardian management,
// approve/cancel, status).
import type { PublicKeyCredentialCreationOptionsJSON, RegistrationResponseJSON } from '@simplewebauthn/browser'
import { request } from './http.js'
import { requestFromWire, type RecoveryRequest, type RecoveryRequestWire } from './accountSession/recovery.js'

export interface StartRecoveryRequest {
  identityId: string
  deviceLabel?: string
}

export interface RecoveryStartResult {
  ticketId: string
  challenge: { publicKey: PublicKeyCredentialCreationOptionsJSON }
}
interface RecoveryStartResultWire {
  ticket_id: string
  challenge: { publicKey: PublicKeyCredentialCreationOptionsJSON }
}

/** `POST /recovery/requests/start` — begins recovering `identityId` on a
 * brand-new device by registering a fresh passkey for it; never sends a
 * bearer token, matching `finishRecoveryRequest`. */
export async function startRecoveryRequest(
  serverUrl: string,
  body: StartRecoveryRequest,
): Promise<RecoveryStartResult> {
  const w = await request<RecoveryStartResultWire>(serverUrl, '/recovery/requests/start', {
    method: 'POST',
    body: { identity_id: body.identityId, device_label: body.deviceLabel ?? null },
  })
  return { ticketId: w.ticket_id, challenge: w.challenge }
}

export interface FinishRecoveryRequest {
  ticketId: string
  webauthnCredential: RegistrationResponseJSON
}

/** `POST /recovery/requests/finish` — completes the passkey-registration
 * ceremony `startRecoveryRequest` began, creating a `pending_approvals`
 * recovery request for the identity's guardians to act on. */
export async function finishRecoveryRequest(
  serverUrl: string,
  body: FinishRecoveryRequest,
): Promise<RecoveryRequest> {
  const w = await request<RecoveryRequestWire>(serverUrl, '/recovery/requests/finish', {
    method: 'POST',
    body: { ticket_id: body.ticketId, webauthn_credential: body.webauthnCredential },
  })
  return requestFromWire(w)
}

/** `GET /recovery/requests/{id}` — public, unauthenticated status read. */
export async function getRecoveryRequest(serverUrl: string, requestId: string): Promise<RecoveryRequest> {
  const w = await request<RecoveryRequestWire>(serverUrl, `/recovery/requests/${requestId}`)
  return requestFromWire(w)
}

/** `POST /recovery/requests/{id}/finalize` — once the guardian threshold is
 * met and any decision delay has elapsed, mints the new passkey as the
 * identity's active login credential. No bearer token — same premise as
 * `startRecoveryRequest`/`finishRecoveryRequest`. */
export async function finalizeRecoveryRequest(serverUrl: string, requestId: string): Promise<RecoveryRequest> {
  const w = await request<RecoveryRequestWire>(serverUrl, `/recovery/requests/${requestId}/finalize`, {
    method: 'POST',
  })
  return requestFromWire(w)
}

/** `GET /identities/{id}/recovery/status` — public, unauthenticated: lets
 * an unfamiliar device check whether a recovery is already in flight for an
 * identity before starting a duplicate one. */
export async function getIdentityRecoveryStatus(
  serverUrl: string,
  identityId: string,
): Promise<RecoveryRequest | null> {
  const w = await request<RecoveryRequestWire | null>(serverUrl, `/identities/${identityId}/recovery/status`)
  return w ? requestFromWire(w) : null
}
