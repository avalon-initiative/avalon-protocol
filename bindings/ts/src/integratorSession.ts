// IntegratorSession — mirrors the Rust `Session`/C# `Session`
// capability-gated integrator model (crates/sdk/src/lib.rs). Distinct from
// AccountSession: no shared base class, no conversion in either direction
// — an integrator credential must never yield account-level power.
//
// Obtained via `AvalonClient.authenticate(identityToken)`, which exchanges
// an identity's existing session token for a Session scoped to this
// integrator's own granted capabilities (`GET /me` + `GET /me/grants`).
// Every read/write method checks its own required capability client-side
// before making a request — `require()` below — the same fast-fail
// convention every other SDK in this repo uses; the server enforces the
// same thing independently, this is not the security boundary.
import { request } from './http.js'
import { CapabilityNotGrantedError, MissingIssuerCredentialsError } from './errors.js'
import { fromMeResponse, type Identity, type Profile, type MeResponseWire } from './types.js'
import { sign as ed25519Sign, bytesToBase64, base64ToBytes } from './crypto/signing.js'

export type Capability =
  | 'identity.read'
  | 'profile.read'
  | 'friends.read'
  | 'presence.read'
  | 'presence.publish'
  | 'guilds.read'
  | 'guilds.chat'
  | 'guilds.issue'
  | 'achievements.read'
  | 'achievements.issue'
  | 'milestones.issue'
  | 'assets.read'
  | 'assets.issue'
  | 'wallet.read'
  | 'wallet.write'
  | 'messages.read'
  | 'messages.send'
  | (string & {})

export interface IntegratorSessionInit {
  identity: Identity
  profile: Profile
  granted: Capability[]
  serverUrl: string
  token: string
  integratorKeyId: string
  integratorSlug?: string
  signingKey?: Uint8Array
}

export interface Friend {
  identityId: string
  since: string
}
interface FriendWire {
  identity_id: string
  since: string
}

export interface GuildMembership {
  guildId: string
  roleIndex: number
}
interface GuildMembershipWire {
  guild_id: string
  role_index: number
}

export interface ConversationSummary {
  id: string
  participants: string[]
}

export interface Authenticity {
  status: string
  [key: string]: unknown
}

/** An identity's own attestation, as returned by `GET /me/achievements` —
 * this SDK's copy of the wire shape (no dependency on avalon-chain). */
export interface VerifiedAttestation {
  id: string
  issuer: string
  subject: string
  achievement: string
  issuedAt: string
  authenticity: Authenticity
  validity: { status: string; [key: string]: unknown }
}
interface VerifiedAttestationWire {
  id: string
  issuer: string
  subject: string
  achievement: string
  issued_at: string
  authenticity: Authenticity
  validity: { status: string; [key: string]: unknown }
}

interface ListMyAchievementsResponseWire {
  achievements: VerifiedAttestationWire[]
  next_cursor: string | null
}

interface ChallengeResponseWire {
  challenge_id: string
  nonce: string
}

/** Must match `avalon_protocol::achievements::attestation_signing_bytes`. */
function attestationSigningBytes(issuerRef: string, subject: string, achievement: string): Uint8Array {
  return new TextEncoder().encode(`avalon:achievement.issued:v1:${issuerRef}:${subject}:${achievement}`)
}

export class IntegratorSession {
  /** @internal */ private readonly identityValue: Identity
  /** @internal */ private readonly profileValue: Profile
  /** @internal */ private readonly granted: Set<Capability>
  /** @internal */ private readonly serverUrl: string
  /** @internal */ private readonly token: string
  /** @internal */ private readonly integratorKeyId: string
  /** @internal */ private readonly integratorSlug?: string
  /** @internal */ private readonly signingKey?: Uint8Array

  constructor(init: IntegratorSessionInit) {
    this.identityValue = init.identity
    this.profileValue = init.profile
    this.granted = new Set(init.granted)
    this.serverUrl = init.serverUrl
    this.token = init.token
    this.integratorKeyId = init.integratorKeyId
    this.integratorSlug = init.integratorSlug
    this.signingKey = init.signingKey
  }

  /** This session's own identity — no network call, populated once by
   * `AvalonClient.authenticate`. */
  identity(): Identity {
    return this.identityValue
  }

  /** This session's own profile, as it was when `authenticate` ran — not
   * re-fetched automatically after a later edit made through another
   * client. */
  profile(): Profile {
    return this.profileValue
  }

  /** Whether `capability` was actually granted to this integrator for this
   * identity. */
  hasCapability(capability: Capability): boolean {
    return this.granted.has(capability)
  }

  /** Every capability-gated method calls this first — throws
   * `CapabilityNotGrantedError` without making a request when the
   * capability isn't present in `GET /me/grants`'s response. */
  private require(capability: Capability): void {
    if (!this.granted.has(capability)) {
      throw new CapabilityNotGrantedError(capability)
    }
  }

  private get<T>(path: string): Promise<T> {
    return request<T>(this.serverUrl, path, { token: this.token })
  }

  private getQuery<T>(path: string, query: Record<string, string>): Promise<T> {
    return request<T>(this.serverUrl, path, { token: this.token, query })
  }

  private post<T>(path: string, body: unknown): Promise<T> {
    return request<T>(this.serverUrl, path, { method: 'POST', token: this.token, body })
  }

  /** `friends.read`-gated — `GET /friends`. */
  async friends(): Promise<Friend[]> {
    this.require('friends.read')
    const w = await this.get<FriendWire[]>('/friends')
    return w.map((f) => ({ identityId: f.identity_id, since: f.since }))
  }

  /** `presence.read`-gated — `GET /presence?ids=`. */
  async presenceOf(ids: string[]): Promise<{ identityId: string; status: string }[]> {
    this.require('presence.read')
    if (ids.length === 0) return []
    return this.getQuery('/presence', { ids: ids.join(',') })
  }

  /** `presence.publish`-gated — `PUT /me/presence`. */
  async updatePresence(status: string): Promise<void> {
    this.require('presence.publish')
    await request(this.serverUrl, '/me/presence', {
      method: 'PUT',
      token: this.token,
      body: { status },
    })
  }

  /** `guilds.read`-gated — `GET /me/guilds`. */
  async guilds(): Promise<GuildMembership[]> {
    this.require('guilds.read')
    const w = await this.get<GuildMembershipWire[]>('/me/guilds')
    return w.map((g) => ({ guildId: g.guild_id, roleIndex: g.role_index }))
  }

  /** `guilds.chat`-gated — `GET /guilds/{id}/channels/{channelId}/messages`. */
  async guildMessages(guildId: string, channelId: string, before?: string, limit?: number) {
    this.require('guilds.chat')
    const query: Record<string, string> = {}
    if (before) query.before = before
    if (limit !== undefined) query.limit = String(limit)
    return this.getQuery(`/guilds/${guildId}/channels/${channelId}/messages`, query)
  }

  /** `guilds.chat`-gated — `POST /guilds/{id}/channels/{channelId}/messages`. */
  async sendGuildMessage(guildId: string, channelId: string, body: string) {
    this.require('guilds.chat')
    return this.post(`/guilds/${guildId}/channels/${channelId}/messages`, { body })
  }

  /** `messages.read`-gated — `GET /conversations`. */
  async conversations(): Promise<ConversationSummary[]> {
    this.require('messages.read')
    return this.get('/conversations')
  }

  /** `messages.send`-gated — `POST /conversations`. */
  async dm(participants: string[]): Promise<ConversationSummary> {
    this.require('messages.send')
    return this.post('/conversations', { participants })
  }

  /** `achievements.read`-gated — this identity's own attestation history.
   * No `recognition` field: that's computed by this integrator against its
   * own trust policy, per ADR #76, never by this SDK on the caller's
   * behalf. */
  async achievements(): Promise<VerifiedAttestation[]> {
    this.require('achievements.read')
    const page = await this.get<ListMyAchievementsResponseWire>('/me/achievements?limit=200')
    return page.achievements.map((a) => ({
      id: a.id,
      issuer: a.issuer,
      subject: a.subject,
      achievement: a.achievement,
      issuedAt: a.issued_at,
      authenticity: a.authenticity,
      validity: a.validity,
    }))
  }

  /** `achievements.issue`-gated — issues `key` (defined by this
   * integrator) to this session's own identity, signed locally with the
   * integrator's own signing key. Two independent proofs go out: an
   * ephemeral challenge-response proving this key is making this call
   * right now, and a signature embedded in the request body over the
   * attestation's own canonical bytes. Throws `MissingIssuerCredentialsError`
   * without any HTTP call if this integrator has no slug/signingKey
   * configured. */
  async issueAchievement(key: string): Promise<string> {
    this.require('achievements.issue')
    if (!this.integratorSlug || !this.signingKey) {
      throw new MissingIssuerCredentialsError()
    }
    const slug = this.integratorSlug

    const challenge = await request<ChallengeResponseWire>(this.serverUrl, `/integrations/${slug}/challenge`, {
      method: 'POST',
    })
    const nonce = base64ToBytes(challenge.nonce)
    const challengeSignature = ed25519Sign(this.signingKey, nonce)

    const subject = this.identityValue.id
    const issuerRef = `game:${slug}`
    const achievement = `game:${slug}:achievement:${key}`
    const signature = ed25519Sign(this.signingKey, attestationSigningBytes(issuerRef, subject, achievement))

    const response = await request<{ id: string }>(this.serverUrl, `/integrations/${slug}/achievements/${key}/issue`, {
      method: 'POST',
      headers: {
        'x-avalon-integrator-key-id': this.integratorKeyId,
        'x-avalon-integrator-challenge-id': challenge.challenge_id,
        'x-avalon-integrator-signature': bytesToBase64(challengeSignature),
        'x-avalon-identity-id': subject,
        'idempotency-key': crypto.randomUUID(),
      },
      body: { key_id: this.integratorKeyId, signature: bytesToBase64(signature) },
    })
    return response.id
  }
}

export interface AuthenticateOptions {
  serverUrl: string
  identityToken: string
  integratorCredentialKeyId: string
  integratorSlug?: string
  signingKey?: Uint8Array
}

/** Exchanges an identity's existing session token for an `IntegratorSession`
 * scoped to this integrator — `GET /me` + `GET /me/grants`. Mirrors
 * `AvalonClient::authenticate` in the Rust SDK. */
export async function authenticate(options: AuthenticateOptions): Promise<IntegratorSession> {
  const body = await request<MeResponseWire>(options.serverUrl, '/me', { token: options.identityToken })
  const { identity, profile } = fromMeResponse(body)

  let granted: Capability[] = []
  try {
    const grants = await request<{ capabilities: string[] }>(options.serverUrl, '/me/grants', {
      token: options.identityToken,
      headers: { 'x-avalon-integrator-key-id': options.integratorCredentialKeyId },
    })
    granted = grants.capabilities
  } catch {
    // An unrecognized/placeholder key id, or no grants yet — treated as
    // "no grants," not an authentication failure (the token already
    // proved who the identity is).
  }

  return new IntegratorSession({
    identity,
    profile,
    granted,
    serverUrl: options.serverUrl,
    token: options.identityToken,
    integratorKeyId: options.integratorCredentialKeyId,
    integratorSlug: options.integratorSlug,
    signingKey: options.signingKey,
  })
}
