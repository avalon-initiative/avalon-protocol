// Wire types matching crates/server/src/handlers.rs's request/response
// shapes field-for-field. The WebAuthn challenge/credential payloads are
// typed via @simplewebauthn/browser's own JSON types rather than redeclared
// here — see crypto/webauthn.ts.
import type {
  AuthenticationResponseJSON,
  PublicKeyCredentialCreationOptionsJSON,
  PublicKeyCredentialRequestOptionsJSON,
  RegistrationResponseJSON,
} from '@simplewebauthn/browser'

export interface RegisterStartRequest {
  identity_id: string
  display_name: string
}

export interface RegisterStartResponse {
  ticket_id: string
  // Server sends { publicKey: PublicKeyCredentialCreationOptionsJSON } —
  // camelCase here even though the rest of this response is snake_case,
  // because CreationChallengeResponse (crates/server/src/handlers.rs, from
  // webauthn-rs-proto) mirrors the browser's native CredentialCreationOptions
  // shape, which is camelCase per the WebAuthn spec itself.
  challenge: { publicKey: PublicKeyCredentialCreationOptionsJSON }
}

export interface RegisterFinishRequest {
  ticket_id: string
  webauthn_credential: RegistrationResponseJSON
  event_signing_public_key: string
  event_signature: string
}

export interface RegisterFinishResponse {
  identity_id: string
}

export interface SessionStartRequest {
  identity_id: string
}

export interface SessionStartResponse {
  ticket_id: string
  // Server's RequestChallengeResponse: { publicKey: ..., mediation? } — same
  // camelCase-nested-in-snake_case shape as RegisterStartResponse above.
  challenge: { publicKey: PublicKeyCredentialRequestOptionsJSON; mediation?: string }
}

export interface SessionFinishRequest {
  ticket_id: string
  credential: AuthenticationResponseJSON
}

export interface SessionFinishResponse {
  token: string
  expires_at: string
}

export interface ProfileResponse {
  identity_id: string
  identity_created_at: string
  display_name: string
  avatar_url: string | null
  // `display_name#discriminator` (issue #128) — the short handle players
  // share with each other instead of a raw identity id.
  handle: string
}

export interface UpdateProfileRequest {
  display_name?: string
  avatar_url?: string
}

// Friends/presence wire types (issue #18), matching
// crates/server/src/friends.rs and crates/server/src/presence.rs
// field-for-field — plain snake_case, unlike the WebAuthn challenge shapes
// above.

export interface FriendshipResponse {
  a: string
  b: string
  since: string
}

export interface CreateFriendRequestRequest {
  to: string
}

export interface FriendRequestResponse {
  id: string
  from: string
  to: string
  requested_at: string
}

export interface ResolveHandleResponse {
  identity_id: string
}

export type PresenceStatus = 'Online' | 'Away' | 'Offline'

export interface PresenceResponse {
  identity_id: string
  status: PresenceStatus
  // Always null today — no game-side presence-publish path exists yet
  // (see #16's scope cut, and #18's own correction note on the issue).
  playing: string | null
  updated_at: string
}
