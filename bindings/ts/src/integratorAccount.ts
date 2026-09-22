// Integrator-account-credential-authenticated writes — the surface an
// integrator/game/app/service manages about *itself* (registration, issuer
// keys, achievement/milestone definitions, Integrator Space schemas/
// mappings/instance data, recognitions, attestation revocation, network
// issuer registration), proven entirely by that integrator's own Ed25519
// key(s) — never an identity session token, never `IntegratorSession`
// (which is scoped to an *identity's* grant of capabilities to an
// integrator, and requires one). Free-standing functions, not methods on
// any session class, the same reasoning `crossNodeLogin.ts`/`recovery.ts`
// already give: there is no session for this actor to hang these off of.
// See crates/server/src/integrators.rs/achievements.rs/integrator_schemas.rs/
// integrator_schema_mappings.rs/integrator_data.rs/recognitions.rs/
// attestations.rs/issuer_registration.rs.
import { request } from './http.js'
import { sign as ed25519Sign, bytesToBase64, base64ToBytes, publicKeyFromSecretKey } from './crypto/signing.js'
import type { components } from './generated.js'

/** Performs one `POST /integrations/{slug}/challenge` -> sign-the-nonce
 * round trip and returns the three headers every integrator-key-
 * authenticated write in this module needs — the same challenge-response
 * scheme `IntegratorSession`'s own `issueAchievement` uses, duplicated here
 * rather than shared across the two files since one is a session method and
 * the other a free function taking raw key material directly. */
async function integratorAuthHeaders(
  serverUrl: string,
  slug: string,
  keyId: string,
  signingKey: Uint8Array,
): Promise<Record<string, string>> {
  const challenge = await request<components['schemas']['IntegratorChallengeResponse']>(
    serverUrl,
    `/integrations/${slug}/challenge`,
    { method: 'POST' },
  )
  const nonce = base64ToBytes(challenge.nonce)
  const signature = ed25519Sign(signingKey, nonce)
  return {
    'x-avalon-integrator-key-id': keyId,
    'x-avalon-integrator-challenge-id': challenge.challenge_id,
    'x-avalon-integrator-signature': bytesToBase64(signature),
  }
}

export interface RegisterIntegratorInput {
  slug: string
  name: string
  ownerName: string
  /** `"game"` (default when omitted), `"app"`, or `"service"`. */
  category?: 'game' | 'app' | 'service'
  requestedCapabilities?: string[]
  /** The integrator's initial Ed25519 public key — generate one with
   * `generateSigningKey()` and keep the matching secret key, it is never
   * sent and never recoverable from the server afterward. */
  publicKey: Uint8Array
}

export interface RegisteredIntegrator {
  id: string
  slug: string
  name: string
  ownerName: string
  registeredAt: string
  status: string
  category: string
  requestedCapabilities: string[]
  /** The initial key's own id — required by every other call in this
   * module as `keyId`. */
  credentialKeyId: string
}

/** `POST /integrations` (#29/#293) — registers a brand-new integrator with
 * one initial Ed25519 key. Unauthenticated: anyone may register a slug, on
 * a first-come basis (slugs are forever, never renamed or reused). Only the
 * public key ever reaches the server — the caller must already hold the
 * matching secret key. */
export async function registerIntegrator(
  serverUrl: string,
  input: RegisterIntegratorInput,
): Promise<RegisteredIntegrator> {
  const w = await request<components['schemas']['IntegratorResponse']>(serverUrl, '/integrations', {
    method: 'POST',
    body: {
      slug: input.slug,
      name: input.name,
      owner_name: input.ownerName,
      category: input.category ?? null,
      requested_capabilities: input.requestedCapabilities ?? [],
      initial_key: { algorithm: 'ed25519', public_key: bytesToBase64(input.publicKey) },
    },
  })
  return {
    id: w.id,
    slug: w.slug,
    name: w.name,
    ownerName: w.owner_name,
    registeredAt: w.registered_at,
    status: w.status,
    category: w.category,
    requestedCapabilities: w.requested_capabilities,
    credentialKeyId: w.credential.key_id,
  }
}

/** `GET /integrations/whoami` — proves `keyId`/`signingKey` authenticate as
 * a real, currently-valid issuer key, and resolves the integrator id it
 * belongs to. Useful as a post-registration/key-rotation sanity check. */
export async function integratorWhoami(
  serverUrl: string,
  slug: string,
  keyId: string,
  signingKey: Uint8Array,
): Promise<string> {
  const headers = await integratorAuthHeaders(serverUrl, slug, keyId, signingKey)
  const w = await request<components['schemas']['IntegratorWhoamiResponse']>(serverUrl, '/integrations/whoami', {
    headers,
  })
  return w.integrator_id
}

export interface IssuerKeyDetail {
  keyId: string
  algorithm: string
  role: string
  purpose: string
  validFrom: string
  validUntil?: string
  revokedAt?: string
}
function issuerKeyDetailFromWire(w: components['schemas']['IssuerKeyResponse']): IssuerKeyDetail {
  return {
    keyId: w.key_id,
    algorithm: w.algorithm,
    role: w.role,
    purpose: w.purpose,
    validFrom: w.valid_from,
    validUntil: w.valid_until ?? undefined,
    revokedAt: w.revoked_at ?? undefined,
  }
}

/** `POST /integrations/{slug}/keys` (#80/#84) — adds a new key to this
 * integrator's key set. Requires a currently-valid **root** key to
 * authenticate (`rootKeyId`/`rootSigningKey`) — an operational key is
 * rejected, since only a root key may authorize a key-set change. */
export async function addIssuerKey(
  serverUrl: string,
  slug: string,
  rootKeyId: string,
  rootSigningKey: Uint8Array,
  input: { publicKey: Uint8Array; role: 'root' | 'operational'; purpose?: 'attestation' | 'shard_settlement'; validUntil?: string },
): Promise<IssuerKeyDetail> {
  const headers = await integratorAuthHeaders(serverUrl, slug, rootKeyId, rootSigningKey)
  const w = await request<components['schemas']['IssuerKeyResponse']>(serverUrl, `/integrations/${slug}/keys`, {
    method: 'POST',
    headers,
    body: {
      algorithm: 'ed25519',
      public_key: bytesToBase64(input.publicKey),
      role: input.role,
      purpose: input.purpose ?? undefined,
      valid_until: input.validUntil ?? null,
    },
  })
  return issuerKeyDetailFromWire(w)
}

/** `POST /integrations/{slug}/keys/{key_id}/revoke` — same root-key
 * authentication requirement as `addIssuerKey`. */
export async function revokeIssuerKey(
  serverUrl: string,
  slug: string,
  rootKeyId: string,
  rootSigningKey: Uint8Array,
  keyIdToRevoke: string,
  reason?: string,
): Promise<IssuerKeyDetail> {
  const headers = await integratorAuthHeaders(serverUrl, slug, rootKeyId, rootSigningKey)
  const w = await request<components['schemas']['IssuerKeyResponse']>(
    serverUrl,
    `/integrations/${slug}/keys/${keyIdToRevoke}/revoke`,
    { method: 'POST', headers, body: { reason: reason ?? null } },
  )
  return issuerKeyDetailFromWire(w)
}

export interface AchievementDefinitionDetail {
  id: string
  integratorId: string
  key: string
  name: string
  description: string
  schema?: string
  icon: string
  iconUrl?: string
  version: number
  createdAt: string
  updatedAt: string
  retired: boolean
  retiredAt?: string
}
function achievementDefinitionDetailFromWire(
  w: components['schemas']['AchievementDefinitionResponse'],
): AchievementDefinitionDetail {
  return {
    id: w.id,
    integratorId: w.integrator_id,
    key: w.key,
    name: w.name,
    description: w.description,
    schema: w.schema ?? undefined,
    icon: w.icon,
    iconUrl: w.icon_url ?? undefined,
    version: w.version,
    createdAt: w.created_at,
    updatedAt: w.updated_at,
    retired: w.retired,
    retiredAt: w.retired_at ?? undefined,
  }
}

export interface CreateClaimDefinitionInput {
  key: string
  name: string
  description: string
  schema?: string
  icon?: string
  iconUrl?: string
}

/** `POST /integrations/{slug}/achievements` — defines a new achievement key
 * for this integrator. Requires this integrator's own (root or operational)
 * key to authenticate. A `key` already defined by this same integrator
 * conflicts; the same `key` defined by a different integrator (or under
 * `createMilestoneDefinition`'s separate vocabulary) is a distinct
 * definition and always succeeds. */
export async function createAchievementDefinition(
  serverUrl: string,
  slug: string,
  keyId: string,
  signingKey: Uint8Array,
  input: CreateClaimDefinitionInput,
): Promise<AchievementDefinitionDetail> {
  const headers = await integratorAuthHeaders(serverUrl, slug, keyId, signingKey)
  const w = await request<components['schemas']['AchievementDefinitionResponse']>(serverUrl, `/integrations/${slug}/achievements`, {
    method: 'POST',
    headers,
    body: claimDefinitionBody(input),
  })
  return achievementDefinitionDetailFromWire(w)
}

/** `POST /integrations/{slug}/milestones` (#324/#325) — same shape as
 * `createAchievementDefinition`, against the separate `milestones` claim
 * vocabulary. */
export async function createMilestoneDefinition(
  serverUrl: string,
  slug: string,
  keyId: string,
  signingKey: Uint8Array,
  input: CreateClaimDefinitionInput,
): Promise<AchievementDefinitionDetail> {
  const headers = await integratorAuthHeaders(serverUrl, slug, keyId, signingKey)
  const w = await request<components['schemas']['AchievementDefinitionResponse']>(serverUrl, `/integrations/${slug}/milestones`, {
    method: 'POST',
    headers,
    body: claimDefinitionBody(input),
  })
  return achievementDefinitionDetailFromWire(w)
}

function claimDefinitionBody(input: CreateClaimDefinitionInput) {
  return {
    key: input.key,
    name: input.name,
    description: input.description,
    schema: input.schema ?? null,
    icon: input.icon ?? null,
    icon_url: input.iconUrl ?? null,
  }
}

export interface UpdateClaimDefinitionInput {
  name?: string
  description?: string
  /** Omit to leave untouched; pass `null` explicitly to the underlying
   * request only ever happens when this is left `undefined` here too — see
   * the module's own convention: every field here means "absent = untouched",
   * `schema` included, despite the wire request requiring the key present. */
  schema?: string
  icon?: string
  iconUrl?: string
  /** `true` to retire the definition — one-way, never used to un-retire. */
  retired?: boolean
}

/** `PATCH /integrations/{slug}/achievements/{key}` — updates name/
 * description/schema/icon (bumping `version`) and/or retires the
 * definition; the id never changes. Requires this integrator's own key to
 * authenticate, same as `createAchievementDefinition`. */
export async function updateAchievementDefinition(
  serverUrl: string,
  slug: string,
  key: string,
  keyId: string,
  signingKey: Uint8Array,
  input: UpdateClaimDefinitionInput,
): Promise<AchievementDefinitionDetail> {
  const headers = await integratorAuthHeaders(serverUrl, slug, keyId, signingKey)
  const w = await request<components['schemas']['AchievementDefinitionResponse']>(
    serverUrl,
    `/integrations/${slug}/achievements/${key}`,
    { method: 'PATCH', headers, body: claimDefinitionUpdateBody(input) },
  )
  return achievementDefinitionDetailFromWire(w)
}

/** `PATCH /integrations/{slug}/milestones/{key}` (#324/#325) — same shape
 * as `updateAchievementDefinition`, against the `milestones` vocabulary. */
export async function updateMilestoneDefinition(
  serverUrl: string,
  slug: string,
  key: string,
  keyId: string,
  signingKey: Uint8Array,
  input: UpdateClaimDefinitionInput,
): Promise<AchievementDefinitionDetail> {
  const headers = await integratorAuthHeaders(serverUrl, slug, keyId, signingKey)
  const w = await request<components['schemas']['AchievementDefinitionResponse']>(
    serverUrl,
    `/integrations/${slug}/milestones/${key}`,
    { method: 'PATCH', headers, body: claimDefinitionUpdateBody(input) },
  )
  return achievementDefinitionDetailFromWire(w)
}

function claimDefinitionUpdateBody(input: UpdateClaimDefinitionInput) {
  return {
    name: input.name ?? null,
    description: input.description ?? null,
    schema: input.schema ?? null,
    icon: input.icon ?? null,
    icon_url: input.iconUrl ?? null,
    retired: input.retired ?? null,
  }
}

export interface PublishSchemaVersionInput {
  protoSource: string
  /** `"public"` (default) or `"private"`. */
  defaultVisibility?: 'public' | 'private'
  fieldVisibility?: Record<string, 'public' | 'private'>
}

/** `POST /integrations/{slug}/schemas` (#384) — publishes the next
 * Integrator Space schema version; always an insert, never an update to an
 * existing version. Requires this integrator's own key to authenticate. */
export async function publishSchemaVersion(
  serverUrl: string,
  slug: string,
  keyId: string,
  signingKey: Uint8Array,
  input: PublishSchemaVersionInput,
): Promise<components['schemas']['IntegratorSchemaVersionResponse']> {
  const headers = await integratorAuthHeaders(serverUrl, slug, keyId, signingKey)
  return request(serverUrl, `/integrations/${slug}/schemas`, {
    method: 'POST',
    headers,
    body: {
      proto_source: input.protoSource,
      default_visibility: input.defaultVisibility ?? 'public',
      field_visibility: input.fieldVisibility ?? {},
    },
  })
}

export interface PublishMappingInput {
  fromSchemaId: string
  toSchemaId: string
  description?: string
  fieldCorrespondence?: Record<string, string>
}

/** `POST /integrations/{slug}/mappings` (#491) — publishes a mapping
 * between two of this integrator's own already-published schema versions.
 * Documents a correspondence only; never executed or interpreted. Requires
 * this integrator's own key to authenticate. */
export async function publishMapping(
  serverUrl: string,
  slug: string,
  keyId: string,
  signingKey: Uint8Array,
  input: PublishMappingInput,
): Promise<components['schemas']['IntegratorSchemaMappingResponse']> {
  const headers = await integratorAuthHeaders(serverUrl, slug, keyId, signingKey)
  return request(serverUrl, `/integrations/${slug}/mappings`, {
    method: 'POST',
    headers,
    body: {
      from_schema_id: input.fromSchemaId,
      to_schema_id: input.toSchemaId,
      description: input.description ?? '',
      field_correspondence: input.fieldCorrespondence ?? {},
    },
  })
}

/** `POST /integrations/{slug}/schemas/{version}/data` (#384) — publishes
 * (or supersedes) this integrator's Integrator Space instance data for
 * `subjectIdentityId` against schema `version`. `subjectIdentityId` must
 * already have an active binding to this integrator, and `instance` must
 * conform to the schema's own parsed root message. Requires this
 * integrator's own key to authenticate. */
export async function publishInstance(
  serverUrl: string,
  slug: string,
  version: number,
  keyId: string,
  signingKey: Uint8Array,
  subjectIdentityId: string,
  instance: Record<string, unknown>,
): Promise<components['schemas']['IntegratorDataInstanceResponse']> {
  const headers = await integratorAuthHeaders(serverUrl, slug, keyId, signingKey)
  return request(serverUrl, `/integrations/${slug}/schemas/${version}/data`, {
    method: 'POST',
    headers,
    body: { subject: subjectIdentityId, instance },
  })
}

/** `DELETE /integrations/{slug}/schemas/{version}/data/{subject}` (#533) —
 * an append-only tombstone: the original instance row is never mutated,
 * only marked deleted (`docs/architecture/revocation.md`'s pattern). The
 * original publish event, and the deletion event this appends, both stay
 * observable in raw ledger history. Requires this integrator's own key to
 * authenticate. */
export async function deleteInstance(
  serverUrl: string,
  slug: string,
  version: number,
  subjectIdentityId: string,
  keyId: string,
  signingKey: Uint8Array,
  reasonCode?: string,
  reason?: string,
): Promise<void> {
  const headers = await integratorAuthHeaders(serverUrl, slug, keyId, signingKey)
  await request(serverUrl, `/integrations/${slug}/schemas/${version}/data/${subjectIdentityId}`, {
    method: 'DELETE',
    headers,
    body: { reason_code: reasonCode ?? 'deleted', reason: reason ?? null },
  })
}

/** `POST /integrations/{slug}/recognitions` (#89) — publishes (or updates)
 * `slug`'s recognition of `recognizedSlug`, for `scope`. Always the
 * caller's own declared policy about itself; upserted, not append-only —
 * republishing updates `scope`/`publishedAt` in place and clears any prior
 * revocation. Requires this integrator's own key to authenticate. */
export async function publishRecognition(
  serverUrl: string,
  slug: string,
  keyId: string,
  signingKey: Uint8Array,
  recognizedSlug: string,
  scope: string[],
): Promise<components['schemas']['RecognitionResponse']> {
  const headers = await integratorAuthHeaders(serverUrl, slug, keyId, signingKey)
  return request(serverUrl, `/integrations/${slug}/recognitions`, {
    method: 'POST',
    headers,
    body: { recognized_slug: recognizedSlug, scope },
  })
}

/** `POST /integrations/{slug}/recognitions/revoke` — marks `slug`'s
 * recognition of `recognizedSlug` revoked (row kept, `revokedAt` set). A
 * no-op, not an error, if no recognition was ever published — check the
 * returned `revoked` flag. Requires this integrator's own key to
 * authenticate. */
export async function revokeRecognition(
  serverUrl: string,
  slug: string,
  keyId: string,
  signingKey: Uint8Array,
  recognizedSlug: string,
): Promise<{ revoked: boolean }> {
  const headers = await integratorAuthHeaders(serverUrl, slug, keyId, signingKey)
  return request(serverUrl, `/integrations/${slug}/recognitions/revoke`, {
    method: 'POST',
    headers,
    body: { recognized_slug: recognizedSlug },
  })
}

/** The exact bytes an issuer's key signs to authorize a revocation. Must stay
 * byte-for-byte identical to the server's own construction
 * (`avalon_protocol::achievements::revocation_signing_bytes`) and to the Rust/C#
 * SDKs' — checked against `conformance/vectors/attestation-signing.json`.
 * Exported for that runner, not part of the public SDK surface. */
export function revocationSigningBytes(
  claimKind: 'achievement' | 'milestone',
  issuerRef: string,
  attestationId: string,
  reasonCode: string,
): Uint8Array {
  return new TextEncoder().encode(`avalon:${claimKind}.revoked:v1:${issuerRef}:${attestationId}:${reasonCode}`)
}

export interface RevokeAttestationResult {
  attestationId: string
  revokedAt: string
  reasonCode: string
  reason: string
}

/** `POST /attestations/{id}/revoke` (#85) — only the attestation's original
 * issuer may revoke it. Two independent proofs, mirroring issuance: the
 * challenge-response scheme every issuer-credentialed endpoint uses, plus
 * an embedded signature over the revocation's own canonical bytes. Callers
 * must supply `issuerRef` (e.g. `"game:ashen-realms"`) and `claimKind`
 * (`"achievement"` or `"milestone"`) — both are already known from the
 * attestation being revoked (`getAttestation`'s own `issuer`/`achievement`
 * fields; `achievement`'s third `:`-separated segment is the claim kind). */
export async function revokeAttestation(
  serverUrl: string,
  attestationId: string,
  claimKind: 'achievement' | 'milestone',
  issuerRef: string,
  issuerSlug: string,
  keyId: string,
  signingKey: Uint8Array,
  reasonCode: string,
  reason: string,
): Promise<RevokeAttestationResult> {
  const headers = await integratorAuthHeaders(serverUrl, issuerSlug, keyId, signingKey)
  const signature = ed25519Sign(
    signingKey,
    revocationSigningBytes(claimKind, issuerRef, attestationId, reasonCode),
  )
  const w = await request<components['schemas']['RevocationResponse']>(
    serverUrl,
    `/attestations/${attestationId}/revoke`,
    {
      method: 'POST',
      headers,
      body: { key_id: keyId, signature: bytesToBase64(signature), reason_code: reasonCode, reason },
    },
  )
  return { attestationId: w.attestation_id, revokedAt: w.revoked_at, reasonCode: w.reason_code, reason: w.reason }
}

/** `POST /issuers/registration-challenge` (#481) — issues a short-lived,
 * single-use nonce for `registerIssuer` to sign proof-of-possession over.
 * Unauthenticated: obtaining a challenge proves nothing by itself. */
export async function createRegistrationChallenge(
  serverUrl: string,
  issuerPublicKey: Uint8Array,
): Promise<{ challengeId: string; nonce: string; expiresAt: string }> {
  const w = await request<components['schemas']['RegistrationChallengeResponse']>(
    serverUrl,
    '/issuers/registration-challenge',
    { method: 'POST', body: { issuer_pubkey: bytesToBase64(issuerPublicKey) } },
  )
  return { challengeId: w.challenge_id, nonce: w.nonce, expiresAt: w.expires_at }
}

export interface IssuerRegistration {
  issuerRef: string
  registeredAt: string
}

/** `POST /issuers/register` (#481) — per-network issuer admission,
 * independent of `crates/sdk`'s own network-verification layer (this SDK
 * has none of its own yet, see `ledger.ts`'s doc comment on the same
 * omission) — callers must independently confirm `declaredNetworkId`
 * really is the network they mean to write to before calling this.
 * Idempotent: re-registering an already-registered key just updates its
 * `issuerRef`. Drives the full challenge -> sign -> register round trip. */
export async function registerIssuer(
  serverUrl: string,
  issuerRef: string,
  declaredNetworkId: string,
  signingKey: Uint8Array,
): Promise<IssuerRegistration> {
  const publicKey = publicKeyFromSecretKey(signingKey)
  const challenge = await createRegistrationChallenge(serverUrl, publicKey)
  const message = new TextEncoder().encode(
    `avalon:issuer.registered:v1:${issuerRef}:${declaredNetworkId}:${challenge.nonce}`,
  )
  const signature = ed25519Sign(signingKey, message)
  const w = await request<components['schemas']['IssuerRegistrationResponse']>(serverUrl, '/issuers/register', {
    method: 'POST',
    body: {
      issuer_pubkey: bytesToBase64(publicKey),
      issuer_ref: issuerRef,
      declared_network_id: declaredNetworkId,
      challenge_id: challenge.challengeId,
      proof_of_possession_signature: bytesToBase64(signature),
    },
  })
  return { issuerRef: w.issuer_ref, registeredAt: w.registered_at }
}
