// Orchestrates the full registration/login ceremonies: API calls + the
// browser WebAuthn ceremony + Ed25519 signing, so views stay glue-only.
import { runAuthenticationCeremony, runRegistrationCeremony } from '../crypto/webauthn'
import {
  bytesToBase64,
  generateAndStoreSigningKey,
  identityCreatedSigningBytes,
  recoverAndStoreSigningKey,
  signWithKey,
} from '../crypto/signingKey'
import * as api from './client'

function newIdentityId(): string {
  return crypto.randomUUID()
}

export interface CreateIdentityResult {
  identityId: string
  // The BIP39 recovery phrase (#134) the signing key was derived from —
  // shown to the player exactly once, immediately after this call returns.
  // Never stored anywhere; the caller's own state is the only copy once
  // this function returns.
  signingKeyMnemonic: string
}

export async function createIdentity(displayName: string): Promise<CreateIdentityResult> {
  const identityId = newIdentityId()

  const { ticket_id, challenge } = await api.registerStart({
    identity_id: identityId,
    display_name: displayName,
  })

  const webauthnCredential = await runRegistrationCeremony(challenge.publicKey)

  const { publicKey, secretKey, mnemonic } = generateAndStoreSigningKey(identityId)
  const signingBytes = identityCreatedSigningBytes(identityId, displayName)
  const signature = signWithKey(secretKey, signingBytes)

  const { identity_id } = await api.registerFinish({
    ticket_id,
    webauthn_credential: webauthnCredential,
    event_signing_public_key: bytesToBase64(publicKey),
    event_signature: bytesToBase64(signature),
  })

  return { identityId: identity_id, signingKeyMnemonic: mnemonic }
}

// Re-derives and re-stores the signing key for an existing identity from a
// previously saved recovery phrase (#134) — the "new device" / "cleared
// storage" path. Does not touch WebAuthn/login at all: recovering the
// signing key and logging in are unrelated (see crypto/signingKey.ts's
// module docs and #99).
export function recoverSigningKey(identityId: string, mnemonic: string): void {
  recoverAndStoreSigningKey(identityId, mnemonic)
}

export interface LoginResult {
  token: string
  expiresAt: string
}

export async function login(identityId: string): Promise<LoginResult> {
  const { ticket_id, challenge } = await api.sessionStart({ identity_id: identityId })

  const credential = await runAuthenticationCeremony(challenge.publicKey)

  const { token, expires_at } = await api.sessionFinish({ ticket_id, credential })

  return { token, expiresAt: expires_at }
}
