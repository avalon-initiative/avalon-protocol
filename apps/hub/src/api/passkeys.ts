// Multi-passkey registration (issue #200) — orchestrates the authenticated
// "add another passkey" WebAuthn ceremony, mirroring api/identity.ts's
// createIdentity() but gated by an existing session instead of being the
// account-creation path. Kept separate from api/deviceGrants.ts on purpose:
// that module manages identity_signing_keys (event-authorship keys, #135);
// this one manages identity_keys (WebAuthn login credentials) — see
// crates/server/src/passkeys.rs's own module doc comment for why they're
// not the same table.
import { runRegistrationCeremony, signFreshAction } from '@avalon/api-client'
import * as api from '@avalon/api-client'
import type { PasskeyResponse } from '@avalon/api-client'

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

// #697/#698/#704: revoking the identity's *last* remaining passkey now
// requires a fresh signature (replacing the old `?confirm=true` query
// param) — signs whenever this device has a local key available; revoking
// one of several passkeys stays ambient and the signature goes unused
// server-side.
export function revokePasskey(
  token: string,
  identityId: string,
  signingKeyId: string | null,
  passkeyId: string,
): Promise<void> {
  const signed = signFreshAction(identityId, signingKeyId, 'passkey.revoke_last', [
    passkeyId,
    identityId,
  ])
  return api.revokePasskey(token, passkeyId, signed ?? {})
}
