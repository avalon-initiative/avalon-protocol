// Multi-passkey management on AccountSession — WebAuthn login
// credentials, distinct from devices.ts (event-signing keys). See
// crates/server/src/passkeys.rs.
import { AccountSession } from './core.js'
import { runRegistrationCeremony } from '../crypto/webauthn.js'
import type { PublicKeyCredentialCreationOptionsJSON, RegistrationResponseJSON } from '@simplewebauthn/browser'
import type { components } from '../generated.js'

export interface Passkey {
  id: string
  label: string | null
  addedAt: string
}

type PasskeyWire = components['schemas']['PasskeyResponse']

function fromWire(w: PasskeyWire): Passkey {
  return { id: w.id, label: w.label ?? null, addedAt: w.added_at }
}

declare module './core.js' {
  interface AccountSession {
    /** `GET /me/passkeys` — every passkey registered to this identity. */
    listPasskeys(): Promise<Passkey[]>
    /** Registers an additional passkey, driving a real browser WebAuthn
     * registration ceremony (`POST /me/passkeys/register/start` -> ceremony
     * -> `POST /me/passkeys/register/finish`). */
    addPasskey(label?: string): Promise<Passkey>
    /** `PATCH /me/passkeys/{id}` — relabels a passkey. Not
     * signature-required. */
    renamePasskey(passkeyId: string, label: string): Promise<Passkey>
    /** `POST /me/passkeys/{id}/revoke` — always signs
     * (`passkey.revoke_last`, `[passkeyId, identityId]`), whether or not
     * this is actually the identity's last remaining passkey. */
    revokePasskey(passkeyId: string): Promise<void>
  }
}

AccountSession.prototype.listPasskeys = async function (this: AccountSession): Promise<Passkey[]> {
  const wire = await this.get<PasskeyWire[]>('/me/passkeys')
  // A 200 with an unexpected (non-array) body is passed through as-is
  // rather than crashing on `.map` — callers that want to treat a
  // malformed response as a real failure (rather than silently trusting
  // it) check `Array.isArray` themselves, same as apps/hub's Profile.vue.
  return Array.isArray(wire) ? wire.map(fromWire) : (wire as unknown as Passkey[])
}

AccountSession.prototype.addPasskey = async function (this: AccountSession, label?: string): Promise<Passkey> {
  const start = await this.postEmpty<{
    ticket_id: string
    challenge: { publicKey: PublicKeyCredentialCreationOptionsJSON }
  }>('/me/passkeys/register/start')
  const credential: RegistrationResponseJSON = await runRegistrationCeremony(start.challenge.publicKey)
  const wire = await this.post<PasskeyWire>('/me/passkeys/register/finish', {
    ticket_id: start.ticket_id,
    webauthn_credential: credential,
    label: label ?? null,
  })
  return fromWire(wire)
}

AccountSession.prototype.renamePasskey = async function (
  this: AccountSession,
  passkeyId: string,
  label: string,
): Promise<Passkey> {
  const wire = await this.patch<PasskeyWire>(`/me/passkeys/${passkeyId}`, { label })
  return fromWire(wire)
}

AccountSession.prototype.revokePasskey = async function (this: AccountSession, passkeyId: string): Promise<void> {
  const signature = this.sign('passkey.revoke_last', [passkeyId, this.identity().id])
  await this.postNoResponse(`/me/passkeys/${passkeyId}/revoke`, signature)
}
