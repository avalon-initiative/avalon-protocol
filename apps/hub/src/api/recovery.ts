// Social recovery (issue #201) — orchestrates the two flows
// crates/server/src/recovery.rs exposes: guardian configuration
// (session-authenticated, mirrors api/passkeys.ts's shape — still on
// @avalon/api-client until Profile.vue's own #712 migration batch) and
// recovery initiation (deliberately unauthenticated — the caller has no
// session for the identity being recovered, so `startRecovery` below never
// takes a token, unlike every other function in this module. Migrated onto
// @avalon/sdk's free-standing recovery.ts as part of RecoverIdentity.vue's
// #712 migration, since it's the only consumer.
import { signFreshAction } from '@avalon/api-client'
import * as api from '@avalon/api-client'
import type {
  GuardianOfSummary,
  GuardianRequestSummary,
  GuardianSettingsResponse,
  RecoveryRequestResponse,
} from '@avalon/api-client'
import {
  startRecoveryRequest as sdkStartRecoveryRequest,
  finishRecoveryRequest as sdkFinishRecoveryRequest,
  finalizeRecoveryRequest as sdkFinalizeRecoveryRequest,
  getIdentityRecoveryStatus as sdkGetIdentityRecoveryStatus,
  runRegistrationCeremony as sdkRunRegistrationCeremony,
  type RecoveryRequest,
} from '@avalon/sdk'
import { getServerUrl } from './serverUrl'

export function getGuardians(token: string): Promise<GuardianSettingsResponse> {
  return api.getGuardians(token)
}

// #697/#698: removing a guardian or raising the threshold is signature-
// required server-side — signs whenever this device has a local key
// available (harmless when the server doesn't actually need it for a
// purely-additive change) rather than replicating the removal/raise check
// client-side.
export function setGuardians(
  token: string,
  identityId: string,
  signingKeyId: string | null,
  guardianIds: string[],
  threshold: number,
): Promise<GuardianSettingsResponse> {
  const signed = signFreshAction(identityId, signingKeyId, 'recovery.guardians.set', [
    identityId,
    [...guardianIds].sort().join(','),
    String(threshold),
  ])
  return api.setGuardians(token, { guardian_ids: guardianIds, threshold, ...signed })
}

export function getMyRecoveryStatus(token: string): Promise<RecoveryRequestResponse | null> {
  return api.getMyRecoveryStatus(token)
}

export function getGuardianRequests(token: string): Promise<GuardianRequestSummary[]> {
  return api.getGuardianRequests(token)
}

// Issue #443: identities relying on the caller as a guardian, and the
// caller's own opt-out self-removal from one of those designations.
export function getGuardianOf(token: string): Promise<GuardianOfSummary[]> {
  return api.getGuardianOf(token)
}

export function resignAsGuardian(token: string, identityId: string): Promise<void> {
  return api.resignAsGuardian(token, identityId)
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

export function finalizeRecoveryRequest(requestId: string): Promise<RecoveryRequest> {
  return sdkFinalizeRecoveryRequest(getServerUrl(), requestId)
}

export function getIdentityRecoveryStatus(identityId: string): Promise<RecoveryRequest | null> {
  return sdkGetIdentityRecoveryStatus(getServerUrl(), identityId)
}

// Drives the new device's WebAuthn registration ceremony end-to-end — same
// two-step start/finish shape `addPasskey` (api/passkeys.ts) uses, but
// unauthenticated throughout: no bearer token exists yet for this device
// against this identity, that's the entire point of recovery.
export async function startRecovery(
  identityId: string,
  deviceLabel?: string,
): Promise<RecoveryRequest> {
  const serverUrl = getServerUrl()
  const { ticketId, challenge } = await sdkStartRecoveryRequest(serverUrl, {
    identityId,
    deviceLabel,
  })

  const webauthnCredential = await sdkRunRegistrationCeremony(challenge.publicKey)

  return sdkFinishRecoveryRequest(serverUrl, {
    ticketId,
    webauthnCredential,
  })
}
