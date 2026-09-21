// Drives the real browser WebAuthn ceremonies via @simplewebauthn/browser,
// reimplemented from packages/api-client/src/crypto/webauthn.ts's pattern
// (not imported — #701's hard invariant is zero dependency on that
// package). @simplewebauthn/browser handles the base64url<->ArrayBuffer
// conversion between the JSON wire format avalon-server speaks
// (webauthn-rs/passkey-types) and the browser's native
// navigator.credentials API.
import { startAuthentication, startRegistration } from '@simplewebauthn/browser'
import type {
  AuthenticationResponseJSON,
  PublicKeyCredentialCreationOptionsJSON,
  PublicKeyCredentialRequestOptionsJSON,
  RegistrationResponseJSON,
} from '@simplewebauthn/browser'

export function runRegistrationCeremony(
  optionsJSON: PublicKeyCredentialCreationOptionsJSON,
): Promise<RegistrationResponseJSON> {
  return startRegistration({ optionsJSON })
}

export function runAuthenticationCeremony(
  optionsJSON: PublicKeyCredentialRequestOptionsJSON,
): Promise<AuthenticationResponseJSON> {
  return startAuthentication({ optionsJSON })
}
