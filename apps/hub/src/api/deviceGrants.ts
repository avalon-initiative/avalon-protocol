// Device-registration / linked-device grant model (issue #135) —
// orchestration split out of Profile.vue so the view stays glue-only, same
// pattern api/friends.ts and api/identity.ts already establish.
//
// Two independent roles a device can play, both handled here:
//   - The *requesting* device (no local signing key yet) generates a fresh
//     keypair, asks for a grant, and polls until it's approved.
//   - An already-*trusted* device (has a local signing key) signs an
//     approval for someone else's pending request.
// Neither role ever sends a private key over the network — only public
// keys and signatures, matching #134's "server never sees private key
// material" invariant, now per-device.
import * as api from './client'
import type { DeviceGrantResponse } from './types'
import {
  base64ToBytes,
  bytesToBase64,
  deviceGrantApprovalSigningBytes,
  generateGrantRequestKeyPair,
  publicKeyFromSecretKey,
  signWithKey,
  storeSigningKey,
} from '../crypto/signingKey'

export interface PendingDeviceGrantRequest {
  grant: DeviceGrantResponse
  // Kept in memory only — never persisted until the grant is actually
  // approved (see `finalizeApprovedGrant`). Losing this (e.g. a page
  // reload) simply means starting a new request; the abandoned pending
  // grant expires server-side on its own.
  secretKey: Uint8Array
}

/** Generates a fresh device keypair and asks the server for a grant. */
export async function beginDeviceGrantRequest(
  token: string,
  deviceLabel: string | null,
): Promise<PendingDeviceGrantRequest> {
  const { secretKey, publicKey } = generateGrantRequestKeyPair()
  const grant = await api.requestDeviceGrant(token, {
    requested_signing_public_key: bytesToBase64(publicKey),
    device_label: deviceLabel,
  })
  return { grant, secretKey }
}

/** Persists the requesting device's key once its grant has actually been approved. */
export function finalizeApprovedGrant(identityId: string, secretKey: Uint8Array): void {
  storeSigningKey(identityId, secretKey)
}

/**
 * Finds this device's own `identity_signing_keys` row id by deriving its
 * locally stored secret key's public key and matching it against the
 * server's device list — the server never hands a device its own id
 * directly (`register_finish` returns only the identity id, not the
 * signing-key row id), so this is how an already-trusted device learns
 * which id to pass as `approver_signing_key_id`. Returns `null` if this
 * device's key isn't found among the identity's registered devices at all
 * (shouldn't happen for a key this module itself stored, but a caller
 * should handle it as "can't approve from here" rather than assume).
 */
export async function findMySigningKeyId(
  token: string,
  localSecretKey: Uint8Array,
): Promise<string | null> {
  const myPublicKey = bytesToBase64(publicKeyFromSecretKey(localSecretKey))
  const devices = await api.listDevices(token)
  return devices.find((d) => d.public_key === myPublicKey && !d.revoked_at)?.id ?? null
}

/**
 * An already-trusted device approves someone else's pending grant — signs
 * the grant with its own already-loaded key and submits the approval.
 * `approverSigningKeyId` must be a signing-key id the caller's identity
 * actually owns (the server independently verifies this — see
 * crates/server/src/devices.rs::approve_device_grant — this isn't the
 * security boundary, just what the request needs to say which key to use).
 */
export async function approveDeviceGrant(
  token: string,
  identityId: string,
  grant: DeviceGrantResponse,
  approverSigningKeyId: string,
  approverSecretKey: Uint8Array,
): Promise<void> {
  const signingBytes = deviceGrantApprovalSigningBytes(
    grant.id,
    identityId,
    base64ToBytes(grant.requested_signing_public_key),
  )
  const signature = signWithKey(approverSecretKey, signingBytes)
  await api.approveDeviceGrant(token, grant.id, {
    approver_signing_key_id: approverSigningKeyId,
    signature: bytesToBase64(signature),
  })
}
