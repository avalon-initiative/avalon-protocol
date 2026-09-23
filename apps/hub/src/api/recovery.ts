// Social recovery — the unauthenticated recovery-initiation
// half only (`crates/server/src/recovery.rs`'s unauthenticated endpoints).
// The session-authenticated half (guardian configuration, status,
// approve/cancel) now lives directly on `AccountSession`
// (`@avalon-initiative/protocol-sdk`'s `accountSession/recovery.ts`) — Profile.vue calls
// `session.session.guardians()`/`.setGuardians()`/etc. itself, no wrapper
// needed, since signing moved inside `AccountSession` too.
import {
  startRecoveryRequest as sdkStartRecoveryRequest,
  finishRecoveryRequest as sdkFinishRecoveryRequest,
  finalizeRecoveryRequest as sdkFinalizeRecoveryRequest,
  getIdentityRecoveryStatus as sdkGetIdentityRecoveryStatus,
  runRegistrationCeremony as sdkRunRegistrationCeremony,
  type RecoveryRequest,
} from '@avalon-initiative/protocol-sdk'
import { getServerUrl } from './serverUrl'

export function finalizeRecoveryRequest(requestId: string): Promise<RecoveryRequest> {
  return sdkFinalizeRecoveryRequest(getServerUrl(), requestId)
}

export function getIdentityRecoveryStatus(identityId: string): Promise<RecoveryRequest | null> {
  return sdkGetIdentityRecoveryStatus(getServerUrl(), identityId)
}

// Drives the new device's WebAuthn registration ceremony end-to-end — same
// two-step start/finish shape `addPasskey` (`@avalon-initiative/protocol-sdk` AccountSession)
// uses, but unauthenticated throughout: no bearer token exists yet for
// this device against this identity, that's the entire point of recovery.
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
