// Device-registration / linked-device grant model — the
// requesting side only. A requesting device (no local signing key yet)
// generates a fresh keypair and asks for a grant; Profile.vue then polls
// `session.session.getDeviceGrant(...)` until it's approved. The approving
// side (`AccountSession.approveDeviceGrant`) needs no wrapper — it's a
// direct call from Profile.vue, signing is automatic. Neither role ever
// sends a private key over the network — only public keys and signatures,
// matching #134's "server never sees private key material" invariant, now
// per-device.
import { generateSigningKey, bytesToBase64, type AccountSession, type DeviceGrant } from '@avalon/sdk'

export interface PendingDeviceGrantRequest {
  grant: DeviceGrant
  // Kept in memory only — never persisted until the grant is actually
  // approved (Profile.vue calls `storeSigningKeySeed` itself once it
  // sees `grant.status === 'approved'`). Losing this (e.g. a page
  // reload) simply means starting a new request; the abandoned pending
  // grant expires server-side on its own.
  secretKey: Uint8Array
}

/** Generates a fresh device keypair and asks the server for a grant. */
export async function beginDeviceGrantRequest(
  session: AccountSession,
  deviceLabel?: string,
): Promise<PendingDeviceGrantRequest> {
  const { secretKey, publicKey } = generateSigningKey()
  const grant = await session.requestDeviceGrant(bytesToBase64(publicKey), deviceLabel)
  return { grant, secretKey }
}
