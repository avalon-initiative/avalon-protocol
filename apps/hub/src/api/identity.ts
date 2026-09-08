// Orchestrates the full registration/login ceremonies: API calls + the
// browser WebAuthn ceremony + Ed25519 signing, so views stay glue-only.
import { runAuthenticationCeremony, runRegistrationCeremony } from '../crypto/webauthn'
import {
  bytesToBase64,
  generateAndStoreSigningKey,
  identityCreatedSigningBytes,
  signWithKey,
} from '../crypto/signingKey'
import * as api from './client'

function newIdentityId(): string {
  return crypto.randomUUID()
}

export interface CreateIdentityResult {
  identityId: string
}

export async function createIdentity(displayName: string): Promise<CreateIdentityResult> {
  const identityId = newIdentityId()

  const { ticket_id, challenge } = await api.registerStart({
    identity_id: identityId,
    display_name: displayName,
  })

  const webauthnCredential = await runRegistrationCeremony(challenge.public_key)

  const { publicKey, secretKey } = generateAndStoreSigningKey(identityId)
  const signingBytes = identityCreatedSigningBytes(identityId, displayName)
  const signature = signWithKey(secretKey, signingBytes)

  const { identity_id } = await api.registerFinish({
    ticket_id,
    webauthn_credential: webauthnCredential,
    event_signing_public_key: bytesToBase64(publicKey),
    event_signature: bytesToBase64(signature),
  })

  return { identityId: identity_id }
}

export interface LoginResult {
  token: string
  expiresAt: string
}

export async function login(identityId: string): Promise<LoginResult> {
  const { ticket_id, challenge } = await api.sessionStart({ identity_id: identityId })

  const credential = await runAuthenticationCeremony(challenge.public_key)

  const { token, expires_at } = await api.sessionFinish({ ticket_id, credential })

  return { token, expiresAt: expires_at }
}
