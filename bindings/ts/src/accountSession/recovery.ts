// Social recovery (issue #201/#443) on AccountSession — M-of-N
// guardian-based recovery when every passkey is lost. See
// crates/server/src/recovery.rs.
import { AccountSession } from './core.js'

export interface GuardianSettings {
  guardianIds: string[]
  threshold: number
  updatedAt: string | null
}

interface GuardianSettingsWire {
  guardian_ids: string[]
  threshold: number
  updated_at: string | null
}

export interface RecoveryRequest {
  id: string
  identityId: string
  status: string
  threshold: number
  approvalsCount: number
  requestedAt: string
  delayEndsAt: string | null
}

// Exported for reuse by ../recovery.ts's free-standing recovery-initiation
// functions, which return this same shape but need no AccountSession.
export interface RecoveryRequestWire {
  id: string
  identity_id: string
  status: string
  threshold: number
  approvals_count: number
  requested_at: string
  delay_ends_at: string | null
}

export function requestFromWire(w: RecoveryRequestWire): RecoveryRequest {
  return {
    id: w.id,
    identityId: w.identity_id,
    status: w.status,
    threshold: w.threshold,
    approvalsCount: w.approvals_count,
    requestedAt: w.requested_at,
    delayEndsAt: w.delay_ends_at,
  }
}

export interface GuardianRequest {
  request: RecoveryRequest
  alreadyApproved: boolean
}

interface GuardianRequestWire {
  request: RecoveryRequestWire
  already_approved: boolean
}

export interface GuardianOf {
  identityId: string
  displayName: string
  addedAt: string
}

interface GuardianOfWire {
  identity_id: string
  display_name: string
  added_at: string
}

declare module './core.js' {
  interface AccountSession {
    /** `GET /me/recovery/guardians`. */
    guardians(): Promise<GuardianSettings>
    /** `PUT /me/recovery/guardians` — always signs
     * (`recovery.guardians.set`, `[identityId, sorted guardian ids
     * comma-joined, threshold]`). */
    setGuardians(guardianIds: string[], threshold: number): Promise<GuardianSettings>
    /** `GET /me/recovery/status` — the caller's own in-flight recovery
     * request, if any. */
    myRecoveryStatus(): Promise<RecoveryRequest | null>
    /** `GET /me/recovery/guardian-requests`. */
    guardianRequests(): Promise<GuardianRequest[]>
    /** `GET /me/recovery/guardian-of`. */
    guardianOf(): Promise<GuardianOf[]>
    /** `DELETE /me/recovery/guardian-of/{identityId}` — self-removal, not
     * signature-required. */
    resignAsGuardian(identityId: string): Promise<void>
    /** `POST /recovery/requests/{id}/approve`. */
    approveRecoveryRequest(requestId: string): Promise<RecoveryRequest>
    /** `POST /recovery/requests/{id}/cancel` — the veto path. */
    cancelRecoveryRequest(requestId: string, reason?: string): Promise<RecoveryRequest>
  }
}

AccountSession.prototype.guardians = async function (this: AccountSession): Promise<GuardianSettings> {
  const w = await this.get<GuardianSettingsWire>('/me/recovery/guardians')
  return { guardianIds: w.guardian_ids, threshold: w.threshold, updatedAt: w.updated_at }
}

AccountSession.prototype.setGuardians = async function (
  this: AccountSession,
  guardianIds: string[],
  threshold: number,
): Promise<GuardianSettings> {
  const sorted = [...guardianIds].sort()
  const signature = this.sign('recovery.guardians.set', [this.identity().id, sorted.join(','), String(threshold)])
  const w = await this.put<GuardianSettingsWire>('/me/recovery/guardians', {
    guardian_ids: guardianIds,
    threshold,
    ...signature,
  })
  return { guardianIds: w.guardian_ids, threshold: w.threshold, updatedAt: w.updated_at }
}

AccountSession.prototype.myRecoveryStatus = async function (this: AccountSession): Promise<RecoveryRequest | null> {
  const w = await this.get<RecoveryRequestWire | null>('/me/recovery/status')
  return w ? requestFromWire(w) : null
}

AccountSession.prototype.guardianRequests = async function (this: AccountSession): Promise<GuardianRequest[]> {
  const w = await this.get<GuardianRequestWire[]>('/me/recovery/guardian-requests')
  return w.map((r) => ({ request: requestFromWire(r.request), alreadyApproved: r.already_approved }))
}

AccountSession.prototype.guardianOf = async function (this: AccountSession): Promise<GuardianOf[]> {
  const w = await this.get<GuardianOfWire[]>('/me/recovery/guardian-of')
  return w.map((g) => ({ identityId: g.identity_id, displayName: g.display_name, addedAt: g.added_at }))
}

AccountSession.prototype.resignAsGuardian = async function (this: AccountSession, identityId: string): Promise<void> {
  await this.del(`/me/recovery/guardian-of/${identityId}`)
}

AccountSession.prototype.approveRecoveryRequest = async function (
  this: AccountSession,
  requestId: string,
): Promise<RecoveryRequest> {
  const w = await this.postEmpty<RecoveryRequestWire>(`/recovery/requests/${requestId}/approve`)
  return requestFromWire(w)
}

AccountSession.prototype.cancelRecoveryRequest = async function (
  this: AccountSession,
  requestId: string,
  reason?: string,
): Promise<RecoveryRequest> {
  const w = await this.post<RecoveryRequestWire>(`/recovery/requests/${requestId}/cancel`, {
    reason: reason ?? null,
  })
  return requestFromWire(w)
}
