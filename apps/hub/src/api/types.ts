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

// GET /me/history (issue #121) — the caller's own protocol event history,
// matching crates/server/src/handlers.rs's HistoryEntryResponse.
export interface HistoryEntryResponse {
  event_id: string
  kind: string
  subject: string
  payload: unknown
  timestamp: string
}

// Device-registration / linked-device grant model (issue #135), matching
// crates/server/src/devices.rs field-for-field.

export interface RequestDeviceGrantRequest {
  requested_signing_public_key: string
  device_label: string | null
}

export interface DeviceGrantResponse {
  id: string
  status: 'pending' | 'approved' | 'denied' | 'expired'
  device_label: string | null
  requested_signing_public_key: string
  requested_at: string
  expires_at: string
}

export interface ApproveDeviceGrantRequest {
  approver_signing_key_id: string
  signature: string
}

export interface DeviceResponse {
  id: string
  label: string | null
  public_key: string
  added_at: string
  // Omitted by the server entirely when the device is still active — see
  // ProfileResponse.avatar_url's own precedent for an optional field that
  // isn't always present on the wire.
  revoked_at?: string
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
