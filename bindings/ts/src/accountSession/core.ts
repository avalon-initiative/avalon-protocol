// AccountSession — a first-party client for an identity's own account
// (mirrors #699/#700's Rust/C# AccountSession, built here from scratch for
// #701). Entirely distinct from IntegratorSession: no shared base class, no
// conversion between the two in either direction — an integrator credential
// must never yield account-level power.
//
// Obtained via AvalonClient.register()/login() (a real browser WebAuthn
// ceremony), AvalonClient.resumeAccountSession(token)/
// resumeAccountSessionWithSigningKey(token, seed), or
// AvalonClient.startAccountDeviceLogin().wait() (see deviceLogin.ts).
//
// Every action #697 flags as signature-required is signed automatically
// here using whatever local Ed25519 signing key this session holds (see
// `sign`). A session with no local key (e.g. resumeAccountSession without a
// seed) sends those actions unsigned, matching the Rust/C# SDKs and Hub's
// own NO_REGISTERED_SIGNING_KEY/FRESH_SIGNATURE_REQUIRED handling
// server-side.
//
// Domain methods are split across sibling files (passkeys.ts, devices.ts,
// recovery.ts, social.ts, conversations.ts, guildAdmin.ts, integrations.ts,
// deviceLogin.ts), matching the Rust/C# "one file per domain" convention —
// each attaches its methods to this class's prototype and merges its own
// method signatures into the `AccountSession` interface below via
// `declare module`.
import { request } from '../http.js'
import { canonicalMessage, sign as ed25519Sign, bytesToBase64, type SignatureFields } from '../crypto/signing.js'
import { mintContinuationToken } from '../crypto/continuation.js'
import { fromMeResponse, type Identity, type Profile, type MeResponseWire, type DeviceRowWire } from '../types.js'
import { NoLocalSigningKeyError } from '../errors.js'

export interface AccountSigningKey {
  secretKey: Uint8Array
  publicKey: Uint8Array
  signingKeyId: string
}

/** Local credential material needed to log back into an already-registered
 * identity via `AvalonClient.login` — returned by `AvalonClient.register`.
 * Not needed for `resumeAccountSession`, which only needs a bearer token. */
export interface AccountCredentials {
  identityId: string
  /** Base64-encoded 32-byte Ed25519 signing-key seed. */
  signingKeySecretBase64: string
}

export interface AccountSessionInit {
  identity: Identity
  profile: Profile
  serverUrl: string
  token: string
  signing?: AccountSigningKey
  credentials?: AccountCredentials
}

export class AccountSession {
  /** @internal */ _identity: Identity
  /** @internal */ _profile: Profile
  /** @internal */ _serverUrl: string
  /** @internal */ _token: string
  /** @internal */ _signing?: AccountSigningKey
  /** @internal */ _credentials?: AccountCredentials

  constructor(init: AccountSessionInit) {
    this._identity = init.identity
    this._profile = init.profile
    this._serverUrl = init.serverUrl
    this._token = init.token
    this._signing = init.signing
    this._credentials = init.credentials
  }

  /** This session's own identity, as of construction or the last
   * `refreshProfile()`. */
  identity(): Identity {
    return this._identity
  }

  /** This session's own profile, as of construction or the last
   * `refreshProfile()`. */
  profile(): Profile {
    return this._profile
  }

  /** The session's own bearer token, for a caller that wants to persist it
   * and later call `resumeAccountSession` instead of registering/logging
   * in again. */
  token(): string {
    return this._token
  }

  /** The server-side `identity_signing_keys.id` this session's local
   * signing key resolves to, if it has one at all. */
  signingKeyId(): string | undefined {
    return this._signing?.signingKeyId
  }

  /** The local credential material `AvalonClient.login` needs to log back
   * into this identity from this device later — set only for a session
   * built via `register`/`login` themselves. */
  credentials(): AccountCredentials | undefined {
    return this._credentials
  }

  /** Re-fetches `GET /me` and updates `identity()`/`profile()` in place. */
  async refreshProfile(): Promise<void> {
    const body = await this.get<MeResponseWire>('/me')
    const { identity, profile } = fromMeResponse(body)
    this._identity = identity
    this._profile = profile
  }

  /** `PATCH /me` — not signature-required (profile fields are
   * self-description, not security state). `undefined` leaves a field
   * untouched; `''` clears a three-state field. Updates `profile()` in
   * place on success. */
  async updateProfile(update: ProfileUpdate): Promise<void> {
    const body = await this.patch<MeResponseWire>('/me', {
      display_name: update.displayName,
      avatar_url: update.avatarUrl,
      bio: update.bio,
      favorite_genres: update.favoriteGenres,
      pronouns: update.pronouns,
      banner_url: update.bannerUrl,
      status: update.status,
      links: update.links,
      timezone: update.timezone,
      theme_color: update.themeColor,
      location: update.location,
    })
    const { identity, profile } = fromMeResponse(body)
    this._identity = identity
    this._profile = profile
  }

  /** Signs `actionTag`/`fields` with this session's local key, if it has
   * one. Every signature-required method calls this unconditionally,
   * including for conditionally-signed endpoints (last-passkey revoke,
   * guardian removal/threshold-raise, escalating member-role change) — an
   * unused-but-valid signature is harmless, the same simplification the
   * Hub frontend and every other SDK in this repo already makes. */
  sign(actionTag: string, fields: string[]): SignatureFields {
    if (!this._signing) {
      return { signing_key_id: null, signature: null }
    }
    const message = canonicalMessage(actionTag, fields)
    const signature = ed25519Sign(this._signing.secretKey, message)
    return { signing_key_id: this._signing.signingKeyId, signature: bytesToBase64(signature) }
  }

  /** Signs `message` directly with this session's local key — for call
   * sites (device-grant approval) whose signed bytes predate the
   * generalized `avalon:<tag>:v1:...` shape. Throws if this session holds
   * no local signing key. */
  signRaw(message: Uint8Array): string {
    if (!this._signing) {
      throw new NoLocalSigningKeyError('signRaw')
    }
    return bytesToBase64(ed25519Sign(this._signing.secretKey, message))
  }

  /** @internal Issue #525: mints a continuation token from this session's
   * own in-memory signing key when a request 401s against this session's
   * own token, so AccountSession keeps working across a node handoff
   * without asking the caller to log in again. Simpler than
   * packages/api-client's storage-adapter-based port — this session
   * already holds identityId/signingKeyId/secretKey directly, nothing to
   * read out of a pluggable adapter. Returns `null` (propagates the 401
   * as-is) when this session holds no local signing key, e.g.
   * `resumeAccountSession` without a seed or a device-pairing-login
   * result. */
  private async reconnect(failedToken: string): Promise<string | null> {
    if (!this._signing || failedToken !== this._token) return null
    return mintContinuationToken(this._identity.id, this._signing.signingKeyId, this._signing.secretKey)
  }

  /** @internal */
  private onUnauthorized = (failedToken: string): Promise<string | null> => this.reconnect(failedToken)

  /** @internal */
  get<T>(path: string): Promise<T> {
    return request<T>(this._serverUrl, path, { token: this._token, onUnauthorized: this.onUnauthorized })
  }

  /** @internal */
  getQuery<T>(path: string, query: Record<string, string>): Promise<T> {
    return request<T>(this._serverUrl, path, { token: this._token, query, onUnauthorized: this.onUnauthorized })
  }

  /** @internal */
  post<T>(path: string, body: unknown): Promise<T> {
    return request<T>(this._serverUrl, path, {
      method: 'POST',
      token: this._token,
      body,
      onUnauthorized: this.onUnauthorized,
    })
  }

  /** @internal a POST with no request body at all. */
  postEmpty<T>(path: string): Promise<T> {
    return request<T>(this._serverUrl, path, {
      method: 'POST',
      token: this._token,
      onUnauthorized: this.onUnauthorized,
    })
  }

  /** @internal a POST carrying a body whose handler returns an empty 200. */
  async postNoResponse(path: string, body: unknown): Promise<void> {
    await request<void>(this._serverUrl, path, {
      method: 'POST',
      token: this._token,
      body,
      onUnauthorized: this.onUnauthorized,
    })
  }

  /** @internal same as postNoResponse, with no request body. */
  async postEmptyNoResponse(path: string): Promise<void> {
    await request<void>(this._serverUrl, path, {
      method: 'POST',
      token: this._token,
      onUnauthorized: this.onUnauthorized,
    })
  }

  /** @internal */
  patch<T>(path: string, body: unknown): Promise<T> {
    return request<T>(this._serverUrl, path, {
      method: 'PATCH',
      token: this._token,
      body,
      onUnauthorized: this.onUnauthorized,
    })
  }

  /** @internal */
  put<T>(path: string, body: unknown): Promise<T> {
    return request<T>(this._serverUrl, path, {
      method: 'PUT',
      token: this._token,
      body,
      onUnauthorized: this.onUnauthorized,
    })
  }

  /** @internal a DELETE carrying no request body, discarding the response. */
  async del(path: string): Promise<void> {
    await request<void>(this._serverUrl, path, {
      method: 'DELETE',
      token: this._token,
      onUnauthorized: this.onUnauthorized,
    })
  }

  /** @internal a DELETE carrying a JSON body — three signature-required
   * endpoints take a small body on what used to be a bodyless DELETE. */
  async deleteWithBody(path: string, body: unknown): Promise<void> {
    await request<void>(this._serverUrl, path, {
      method: 'DELETE',
      token: this._token,
      body,
      onUnauthorized: this.onUnauthorized,
    })
  }
}

/** A partial update to an identity's own profile — every field `undefined`
 * means "leave untouched," matching `PATCH /me`'s own convention. Passing
 * `''` on a three-state field (`bio`, `avatarUrl`, etc.) clears it. */
export interface ProfileUpdate {
  displayName?: string
  avatarUrl?: string
  bio?: string
  favoriteGenres?: string[]
  pronouns?: string
  bannerUrl?: string
  status?: string
  links?: string[]
  timezone?: string
  themeColor?: string
  location?: string
}

export async function findOwnSigningKeyId(
  serverUrl: string,
  token: string,
  publicKeyB64: string,
): Promise<string | undefined> {
  const devices = await request<DeviceRowWire[]>(serverUrl, '/me/devices', { token })
  return devices.find((d) => d.public_key === publicKeyB64)?.id
}
