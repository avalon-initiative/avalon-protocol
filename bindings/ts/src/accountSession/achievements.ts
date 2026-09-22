// The caller's own attestation history (issue #34/#35/#377) on
// AccountSession — a player's read of their own achievements, distinct
// from IntegratorSession's achievements(), which is capability-gated and
// issuance-focused. See crates/server/src/attestations.rs.
import { AccountSession } from './core.js'
import type { components } from '../generated.js'

export interface AttestationProof {
  keyId: string
  algorithm: string
}

export interface AttestationAuthenticity {
  status: 'authentic' | 'not_authentic'
  keyId?: string
  reason?: string
}

export interface AttestationValidity {
  status: 'valid' | 'invalid'
  reason?: string
}

export interface AttestationHistoryEntry {
  event: string
  at: string
  reasonCode?: string
  reason?: string
}

/** One issued claim against this identity — active or revoked alike.
 * Deliberately no `recognition` field: per ADR #76, that's a consumer's
 * own trust-policy call, never the server's (or this SDK's). */
export interface Attestation {
  id: string
  issuer: string
  subject: string
  achievement: string
  issuedAt: string
  proof: AttestationProof
  authenticity: AttestationAuthenticity
  validity: AttestationValidity
  history: AttestationHistoryEntry[]
}
type AttestationWire = components['schemas']['AttestationReadResponse']
function attestationFromWire(w: AttestationWire): Attestation {
  return {
    id: w.id,
    issuer: w.issuer,
    subject: w.subject,
    achievement: w.achievement,
    issuedAt: w.issued_at,
    proof: { keyId: w.proof.key_id, algorithm: w.proof.algorithm },
    // `authenticity`/`validity` are real discriminated unions on the wire
    // (status: "authentic" carries key_id, status: "not_authentic" carries
    // reason, etc.) — flattened here to the same optional-both-fields
    // domain shape this SDK already exposed.
    authenticity:
      w.authenticity.status === 'authentic'
        ? { status: 'authentic', keyId: w.authenticity.key_id }
        : { status: 'not_authentic', reason: w.authenticity.reason },
    validity: w.validity.status === 'valid' ? { status: 'valid' } : { status: 'invalid', reason: w.validity.reason },
    history: w.history.map((h) => ({
      event: h.event,
      at: h.at,
      reasonCode: h.reason_code ?? undefined,
      reason: h.reason ?? undefined,
    })),
  }
}

type ListMyAchievementsResponseWire = components['schemas']['ListMyAchievementsResponse']

declare module './core.js' {
  interface AccountSession {
    /** `GET /me/achievements?limit=200` — the server's max page size in one
     * request rather than "load more" pagination, matching the Rust SDK's
     * own #377 scope decision; `next_cursor` isn't exposed here as a
     * result. */
    getMyAchievements(): Promise<Attestation[]>
  }
}

AccountSession.prototype.getMyAchievements = async function (this: AccountSession): Promise<Attestation[]> {
  const w = await this.get<ListMyAchievementsResponseWire>('/me/achievements?limit=200')
  if (!Array.isArray(w?.achievements)) return w as unknown as Attestation[]
  return w.achievements.map(attestationFromWire)
}
