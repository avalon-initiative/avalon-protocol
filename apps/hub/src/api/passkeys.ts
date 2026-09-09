// Multi-passkey registration (issue #200) — orchestrates the authenticated
// "add another passkey" WebAuthn ceremony, mirroring api/identity.ts's
// createIdentity() but gated by an existing session instead of being the
// account-creation path. Kept separate from api/deviceGrants.ts on purpose:
// that module manages identity_signing_keys (event-authorship keys, #135);
// this one manages identity_keys (WebAuthn login credentials) — see
// crates/server/src/passkeys.rs's own module doc comment for why they're
// not the same table.
import { runRegistrationCeremony } from '../crypto/webauthn'
import * as api from './client'
import type { PasskeyResponse } from './types'

export async function addPasskey(token: string, label: string | null): Promise<PasskeyResponse> {
  const { ticket_id, challenge } = await api.startAddPasskey(token)

  const webauthnCredential = await runRegistrationCeremony(challenge.publicKey)

  return api.finishAddPasskey(token, {
    ticket_id,
    webauthn_credential: webauthnCredential,
    label,
  })
}

export function listPasskeys(token: string): Promise<PasskeyResponse[]> {
  return api.listPasskeys(token)
}

export function renamePasskey(token: string, passkeyId: string, label: string): Promise<PasskeyResponse> {
  return api.renamePasskey(token, passkeyId, { label })
}

// `confirm: true` is required to revoke the identity's last remaining
// passkey — the ticket's explicit "I understand this may lock me out"
// invariant. A caller that gets a 409 back (crates/server/src/error.rs's
// `LastPasskeyRequiresConfirmation`) should ask the player to confirm and
// retry with `confirm: true`, not retry silently.
export function revokePasskey(token: string, passkeyId: string, confirm = false): Promise<void> {
  return api.revokePasskey(token, passkeyId, confirm)
}
