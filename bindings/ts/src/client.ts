// AvalonClient — the entry point. Construct one with `new AvalonClient(...)`,
// then either drive an AccountSession (register/login/resumeAccountSession*/
// startAccountDeviceLogin) or an IntegratorSession (authenticate). Mirrors
// the Rust SDK's `AvalonClient` (crates/sdk/src/lib.rs, crates/sdk/src/
// account/mod.rs).
import type {
  PublicKeyCredentialCreationOptionsJSON,
  PublicKeyCredentialRequestOptionsJSON,
  RegistrationResponseJSON,
  AuthenticationResponseJSON,
} from '@simplewebauthn/browser'
import { request } from './http.js'
import { fromMeResponse, type MeResponseWire } from './types.js'
import {
  AccountSession,
  findOwnSigningKeyId,
  startAccountDeviceLoginRequest,
  type AccountCredentials,
  type AccountDeviceLogin,
} from './accountSession/index.js'
import { runRegistrationCeremony, runAuthenticationCeremony } from './crypto/webauthn.js'
import {
  generateSigningKey,
  publicKeyFromSecretKey,
  bytesToBase64,
  base64ToBytes,
  sign as ed25519Sign,
  identityCreatedSigningBytes,
  type SigningKeyPair,
} from './crypto/signing.js'
import { generateMnemonicSigningKey } from './crypto/mnemonic.js'
import { authenticate as authenticateIntegrator, type AuthenticateOptions, type IntegratorSession } from './integratorSession.js'

export interface AvalonClientConfig {
  serverUrl: string
}

interface RegisterStartResponseWire {
  ticket_id: string
  challenge: { publicKey: PublicKeyCredentialCreationOptionsJSON }
}

interface SessionStartResponseWire {
  ticket_id: string
  challenge: { publicKey: PublicKeyCredentialRequestOptionsJSON; mediation?: string }
}

interface SessionFinishResponseWire {
  token: string
  expires_at: string
}

export class AvalonClient {
  private readonly serverUrl: string

  constructor(config: AvalonClientConfig) {
    this.serverUrl = config.serverUrl
  }

  /** Registers a brand-new identity and returns a fresh `AccountSession`
   * for it — drives a real browser WebAuthn registration ceremony via
   * `@simplewebauthn/browser`, generates a fresh Ed25519 event-signing key
   * locally, then immediately logs the new identity in (`POST
   * /sessions/start`/`finish`) so the returned session carries a real
   * bearer token. `session.credentials()` carries what `login()` needs to
   * log back into this same identity later. */
  async register(displayName: string, deviceLabel?: string): Promise<AccountSession> {
    return this.registerWithKeyPair(displayName, deviceLabel, generateSigningKey())
  }

  /** Same as `register()`, but the event-signing key is deterministically
   * derived from a fresh BIP39 recovery phrase (issue #134) rather than
   * generated purely at random — the same disaster-recovery fallback
   * `packages/api-client`'s `createIdentity` already gives Hub's users
   * today. Returns the phrase alongside the session so the caller can
   * show it to the user exactly once; this SDK never persists it
   * anywhere. Use `deriveSigningKeyFromMnemonic`/`isValidMnemonic`
   * (`crypto/mnemonic.js`) later to re-derive the same key from a saved
   * phrase — e.g. via `AccountSession`'s own signing-key resolution, not
   * a second registration. */
  async registerWithMnemonic(
    displayName: string,
    deviceLabel?: string,
  ): Promise<{ session: AccountSession; mnemonic: string }> {
    const { mnemonic, ...keyPair } = generateMnemonicSigningKey()
    const session = await this.registerWithKeyPair(displayName, deviceLabel, keyPair)
    return { session, mnemonic }
  }

  private async registerWithKeyPair(
    displayName: string,
    deviceLabel: string | undefined,
    keyPair: SigningKeyPair,
  ): Promise<AccountSession> {
    const identityId = crypto.randomUUID()
    const { secretKey, publicKey } = keyPair
    const eventSigningPublicKey = bytesToBase64(publicKey)

    const start = await request<RegisterStartResponseWire>(this.serverUrl, '/identities/register/start', {
      method: 'POST',
      body: { identity_id: identityId, display_name: displayName },
    })

    const credential: RegistrationResponseJSON = await runRegistrationCeremony(start.challenge.publicKey)

    const signingBytes = identityCreatedSigningBytes(identityId, displayName)
    const eventSignature = bytesToBase64(ed25519Sign(secretKey, signingBytes))

    await request(this.serverUrl, '/identities/register/finish', {
      method: 'POST',
      body: {
        ticket_id: start.ticket_id,
        webauthn_credential: credential,
        event_signing_public_key: eventSigningPublicKey,
        event_signature: eventSignature,
        device_label: deviceLabel ?? null,
      },
    })

    const credentials: AccountCredentials = {
      identityId,
      signingKeySecretBase64: bytesToBase64(secretKey),
    }

    const session = await this.finishLogin(identityId, secretKey)
    session._credentials = credentials
    return session
  }

  /** Logs back into an identity previously registered via `register()` (on
   * this same browser/device, or one sharing its WebAuthn credential),
   * driving a real `POST /sessions/start`/`finish` WebAuthn ceremony.
   * `credentials` is what `session.credentials()` returned from that
   * earlier registration (or a prior `login()`). */
  async login(credentials: AccountCredentials): Promise<AccountSession> {
    const secretKey = base64ToBytes(credentials.signingKeySecretBase64)
    const session = await this.finishLogin(credentials.identityId, secretKey)
    session._credentials = credentials
    return session
  }

  /** Shared `POST /sessions/start` -> ceremony -> `POST /sessions/finish`
   * -> `GET /me` -> resolve own `signingKeyId` sequence used by both
   * `register()` and `login()`. */
  private async finishLogin(identityId: string, secretKey: Uint8Array): Promise<AccountSession> {
    const start = await request<SessionStartResponseWire>(this.serverUrl, '/sessions/start', {
      method: 'POST',
      body: { identity_id: identityId },
    })

    const assertion: AuthenticationResponseJSON = await runAuthenticationCeremony(start.challenge.publicKey)

    const finish = await request<SessionFinishResponseWire>(this.serverUrl, '/sessions/finish', {
      method: 'POST',
      body: { ticket_id: start.ticket_id, credential: assertion },
    })

    return this.resumeAccountSessionWithSigningKey(finish.token, secretKey)
  }

  /** Resumes an already-minted bearer token as an `AccountSession`. Holds
   * no local signing key, so every signature-required method on the
   * result sends its request unsigned — use
   * `resumeAccountSessionWithSigningKey` instead when this process also
   * holds the identity's signing-key seed. */
  async resumeAccountSession(token: string): Promise<AccountSession> {
    const body = await request<MeResponseWire>(this.serverUrl, '/me', { token })
    const { identity, profile } = fromMeResponse(body)
    return new AccountSession({ identity, profile, serverUrl: this.serverUrl, token })
  }

  /** Same as `resumeAccountSession`, but also resolves `signingKeySeed`'s
   * server-side `signing_key_id` (via `GET /me/devices`, matching on
   * public key) so the returned session's signature-required methods sign
   * automatically. */
  async resumeAccountSessionWithSigningKey(token: string, signingKeySeed: Uint8Array): Promise<AccountSession> {
    const body = await request<MeResponseWire>(this.serverUrl, '/me', { token })
    const { identity, profile } = fromMeResponse(body)
    const publicKey = publicKeyFromSecretKey(signingKeySeed)
    const publicKeyB64 = bytesToBase64(publicKey)
    const signingKeyId = await findOwnSigningKeyId(this.serverUrl, token, publicKeyB64)
    return new AccountSession({
      identity,
      profile,
      serverUrl: this.serverUrl,
      token,
      signing: signingKeyId ? { secretKey: signingKeySeed, publicKey, signingKeyId } : undefined,
    })
  }

  /** Starts a cross-device pairing (#307) via `POST /auth/device/start`,
   * resolving to an `AccountSession` via `.wait()` rather than the
   * integrator `IntegratorSession` `login()`-equivalent resolves to. Use
   * this instead of `register()`/`login()` when this process has no
   * WebAuthn ceremony surface of its own (a game engine, a headless
   * client). */
  async startAccountDeviceLogin(): Promise<AccountDeviceLogin> {
    return startAccountDeviceLoginRequest(this.serverUrl, (token) => this.resumeAccountSession(token))
  }

  /** Exchanges an identity's existing Avalon session token for an
   * `IntegratorSession` scoped to this integrator's own granted
   * capabilities. */
  async authenticate(options: Omit<AuthenticateOptions, 'serverUrl'>): Promise<IntegratorSession> {
    return authenticateIntegrator({ ...options, serverUrl: this.serverUrl })
  }
}

export type { AccountSession, AccountCredentials, AccountDeviceLogin, ProfileUpdate } from './accountSession/index.js'
export type { IntegratorSession, Capability } from './integratorSession.js'
