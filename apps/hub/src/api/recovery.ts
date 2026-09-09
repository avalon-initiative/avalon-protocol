// Social recovery (issue #201) — orchestrates the two flows
// crates/server/src/recovery.rs exposes: guardian configuration
// (session-authenticated, mirrors api/passkeys.ts's shape) and recovery
// initiation (deliberately unauthenticated — the caller has no session for
// the identity being recovered, so `startRecovery` below never takes a
// token, unlike every other function in this module).
import { runRegistrationCeremony } from '../crypto/webauthn'
import * as api from './client'
import type {
  GuardianRequestSummary,
  GuardianSettingsResponse,
  RecoveryRequestResponse,
} from './types'

export function getGuardians(token: string): Promise<GuardianSettingsResponse> {
  return api.getGuardians(token)
}

export function setGuardians(
  token: string,
  guardianIds: string[],
  threshold: number,
): Promise<GuardianSettingsResponse> {
  return api.setGuardians(token, { guardian_ids: guardianIds, threshold })
}

export function getMyRecoveryStatus(token: string): Promise<RecoveryRequestResponse | null> {
  return api.getMyRecoveryStatus(token)
}

export function getGuardianRequests(token: string): Promise<GuardianRequestSummary[]> {
  return api.getGuardianRequests(token)
}

export function approveRecoveryRequest(
  token: string,
  requestId: string,
): Promise<RecoveryRequestResponse> {
  return api.approveRecoveryRequest(token, requestId)
}

export function cancelRecoveryRequest(
  token: string,
  requestId: string,
  reason: string | null = null,
): Promise<RecoveryRequestResponse> {
  return api.cancelRecoveryRequest(token, requestId, { reason })
}

export function finalizeRecoveryRequest(requestId: string): Promise<RecoveryRequestResponse> {
  return api.finalizeRecoveryRequest(requestId)
}

export function getIdentityRecoveryStatus(
  identityId: string,
): Promise<RecoveryRequestResponse | null> {
  return api.getIdentityRecoveryStatus(identityId)
}

// Drives the new device's WebAuthn registration ceremony end-to-end — same
// two-step start/finish shape `addPasskey` (api/passkeys.ts) uses, but
// unauthenticated throughout: no bearer token exists yet for this device
// against this identity, that's the entire point of recovery.
export async function startRecovery(
  identityId: string,
  deviceLabel: string | null,
): Promise<RecoveryRequestResponse> {
  const { ticket_id, challenge } = await api.startRecoveryRequest({
    identity_id: identityId,
    device_label: deviceLabel,
  })

  const webauthnCredential = await runRegistrationCeremony(challenge.publicKey)

  return api.finishRecoveryRequest({
    ticket_id,
    webauthn_credential: webauthnCredential,
  })
}
