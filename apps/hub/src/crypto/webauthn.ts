// Drives the real browser WebAuthn ceremonies via @simplewebauthn/browser,
// which handles the base64url<->ArrayBuffer conversion between the JSON
// wire format avalon-server speaks (webauthn-rs/passkey-types) and the
// browser's native navigator.credentials API. Verified field-for-field
// against crates/server/src/handlers.rs's request/response types before
// picking this library — see issue #55's PR description for that check;
// the shapes line up exactly (camelCase, base64url-no-pad fields, and the
// same clientExtensionResults/extensions aliasing on the response side), no
// adapter needed beyond unwrapping the server's `{ publicKey: ... }`
// envelope before calling in.
import { startAuthentication, startRegistration } from '@simplewebauthn/browser'
import type { AuthenticationResponseJSON, RegistrationResponseJSON } from '@simplewebauthn/browser'

import type {
  PublicKeyCredentialCreationOptionsJSON,
  PublicKeyCredentialRequestOptionsJSON,
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
