// Device-registration / linked-device grant model (issue #135) and
// cross-device pairing approval (issue #307/#704) on AccountSession —
// identity_signing_keys rows (event-authorship keys), distinct from
// passkeys.ts (WebAuthn login credentials). See
// crates/server/src/devices.rs/device_pairing.rs.
import { AccountSession } from './core.js'
import { deviceGrantApprovalSigningBytes } from '../crypto/signing.js'
import { NoLocalSigningKeyError } from '../errors.js'

export interface Device {
  id: string
  label: string | null
  publicKey: string
  addedAt: string
  revokedAt: string | null
}

interface DeviceWire {
  id: string
  label: string | null
  public_key: string
  added_at: string
  revoked_at?: string | null
}

function fromWire(w: DeviceWire): Device {
  return { id: w.id, label: w.label, publicKey: w.public_key, addedAt: w.added_at, revokedAt: w.revoked_at ?? null }
}

export interface DeviceGrant {
  id: string
  status: string
  deviceLabel: string | null
  requestedSigningPublicKey: string
  requestedAt: string
  expiresAt: string
}

interface DeviceGrantWire {
  id: string
  status: string
  device_label: string | null
  requested_signing_public_key: string
  requested_at: string
  expires_at: string
}

function grantFromWire(w: DeviceGrantWire): DeviceGrant {
  return {
    id: w.id,
    status: w.status,
    deviceLabel: w.device_label,
    requestedSigningPublicKey: w.requested_signing_public_key,
    requestedAt: w.requested_at,
    expiresAt: w.expires_at,
  }
}

declare module './core.js' {
  interface AccountSession {
    /** `GET /me/devices` — every signing-key device registered to this
     * identity, active and revoked alike. */
    listDevices(): Promise<Device[]>
    /** `PATCH /me/devices/{signingKeyId}` — relabels a device. Not
     * signature-required. */
    renameDevice(signingKeyId: string, label: string): Promise<Device>
    /** `POST /me/devices/{signingKeyId}/revoke` — unilateral, ambient-token
     * (revocation only ever narrows trust). */
    revokeDevice(signingKeyId: string): Promise<void>
    /** `POST /me/devices/grants` — requests a new device's signing key be
     * added, from the requesting device's own (as-yet unsigned) session. */
    requestDeviceGrant(requestedSigningPublicKeyB64: string, deviceLabel?: string): Promise<DeviceGrant>
    /** `GET /me/devices/grants[?status=]`. */
    listDeviceGrants(status?: string): Promise<DeviceGrant[]>
    /** `GET /me/devices/grants/{id}`. */
    getDeviceGrant(grantId: string): Promise<DeviceGrant>
    /** `POST /me/devices/grants/{id}/approve` — approves a pending grant,
     * signed over `device_grant.approved` bytes proving this device once
     * passed a real WebAuthn ceremony. Throws `NoLocalSigningKeyError` if
     * this session has no local signing key. */
    approveDeviceGrant(grantId: string, requestedSigningPublicKeyB64: string): Promise<Device>
    /** `POST /auth/device/approve` (#307/#704) — approves a cross-device
     * pairing request by `userCode`, always signed
     * (`device_pairing.approve`, `[identityId, userCode]`). */
    approveDevicePairing(userCode: string): Promise<string>
    /** `POST /auth/device/deny` — declines a pairing request. Not
     * signature-required. */
    denyDevicePairing(userCode: string): Promise<string>
  }
}

AccountSession.prototype.listDevices = async function (this: AccountSession): Promise<Device[]> {
  const wire = await this.get<DeviceWire[]>('/me/devices')
  return wire.map(fromWire)
}

AccountSession.prototype.renameDevice = async function (
  this: AccountSession,
  signingKeyId: string,
  label: string,
): Promise<Device> {
  const wire = await this.patch<DeviceWire>(`/me/devices/${signingKeyId}`, { label })
  return fromWire(wire)
}

AccountSession.prototype.revokeDevice = async function (this: AccountSession, signingKeyId: string): Promise<void> {
  await this.postEmptyNoResponse(`/me/devices/${signingKeyId}/revoke`)
}

AccountSession.prototype.requestDeviceGrant = async function (
  this: AccountSession,
  requestedSigningPublicKeyB64: string,
  deviceLabel?: string,
): Promise<DeviceGrant> {
  const wire = await this.post<DeviceGrantWire>('/me/devices/grants', {
    requested_signing_public_key: requestedSigningPublicKeyB64,
    device_label: deviceLabel ?? null,
  })
  return grantFromWire(wire)
}

AccountSession.prototype.listDeviceGrants = async function (
  this: AccountSession,
  status?: string,
): Promise<DeviceGrant[]> {
  const wire = status
    ? await this.getQuery<DeviceGrantWire[]>('/me/devices/grants', { status })
    : await this.get<DeviceGrantWire[]>('/me/devices/grants')
  return wire.map(grantFromWire)
}

AccountSession.prototype.getDeviceGrant = async function (
  this: AccountSession,
  grantId: string,
): Promise<DeviceGrant> {
  const wire = await this.get<DeviceGrantWire>(`/me/devices/grants/${grantId}`)
  return grantFromWire(wire)
}

AccountSession.prototype.approveDeviceGrant = async function (
  this: AccountSession,
  grantId: string,
  requestedSigningPublicKeyB64: string,
): Promise<Device> {
  const signingKeyId = this.signingKeyId()
  if (!signingKeyId) {
    throw new NoLocalSigningKeyError('approveDeviceGrant')
  }
  const message = deviceGrantApprovalSigningBytes(grantId, this.identity().id, requestedSigningPublicKeyB64)
  const signature = this.signRaw(message)
  const wire = await this.post<DeviceWire>(`/me/devices/grants/${grantId}/approve`, {
    approver_signing_key_id: signingKeyId,
    signature,
  })
  return fromWire(wire)
}

AccountSession.prototype.approveDevicePairing = async function (
  this: AccountSession,
  userCode: string,
): Promise<string> {
  const signature = this.sign('device_pairing.approve', [this.identity().id, userCode])
  const response = await this.post<{ status: string }>('/auth/device/approve', { user_code: userCode, ...signature })
  return response.status
}

AccountSession.prototype.denyDevicePairing = async function (this: AccountSession, userCode: string): Promise<string> {
  const response = await this.post<{ status: string }>('/auth/device/deny', { user_code: userCode })
  return response.status
}
