// Public, unauthenticated integrator-directory/registry reads —
// free-standing functions, not session methods, same convention as
// ledger.ts's getLatestSth. See
// crates/server/src/integrations.rs/registry.rs/achievements.rs/
// integrator_schemas.rs/integrator_schema_mappings.rs/recognitions.rs.
import { request } from './http.js'
import type { components } from './generated.js'

export type IntegratorCategory = 'game' | 'app' | 'service'

export interface AchievementDefinition {
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
interface AchievementDefinitionWire {
  id: string
  integrator_id: string
  key: string
  name: string
  description: string
  schema?: string
  icon: string
  icon_url?: string
  version: number
  created_at: string
  updated_at: string
  retired: boolean
  retired_at?: string
}
function achievementDefinitionFromWire(w: AchievementDefinitionWire): AchievementDefinition {
  return {
    id: w.id,
    integratorId: w.integrator_id,
    key: w.key,
    name: w.name,
    description: w.description,
    schema: w.schema,
    icon: w.icon,
    iconUrl: w.icon_url,
    version: w.version,
    createdAt: w.created_at,
    updatedAt: w.updated_at,
    retired: w.retired,
    retiredAt: w.retired_at,
  }
}

/** `GET /integrations/{slug}/achievements` — public, used to resolve a
 * claim's display name (`AttestationResponse` only carries the definition's
 * GlobalId string). */
export async function listAchievementDefinitions(serverUrl: string, slug: string): Promise<AchievementDefinition[]> {
  const w = await request<AchievementDefinitionWire[]>(serverUrl, `/integrations/${slug}/achievements`)
  // Passed through as-is on a non-array body rather than trusting it —
  // same reasoning as every list-returning AccountSession method.
  if (!Array.isArray(w)) return w as unknown as AchievementDefinition[]
  return w.map(achievementDefinitionFromWire)
}

/** `GET /integrations/{slug}/milestones`. */
export async function listMilestoneDefinitions(serverUrl: string, slug: string): Promise<AchievementDefinition[]> {
  const w = await request<AchievementDefinitionWire[]>(serverUrl, `/integrations/${slug}/milestones`)
  if (!Array.isArray(w)) return w as unknown as AchievementDefinition[]
  return w.map(achievementDefinitionFromWire)
}

export interface Integrator {
  id: string
  slug: string
  name: string
  ownerName: string
  registeredAt: string
  status: string
  category: IntegratorCategory
  requestedCapabilities: string[]
}
interface IntegratorWire {
  id: string
  slug: string
  name: string
  owner_name: string
  registered_at: string
  status: string
  category: IntegratorCategory
  requested_capabilities: string[]
}

/** `GET /integrations/{slug}` — public and unauthenticated
 * (crates/server/src/integrations.rs), the same endpoint whether called
 * from a logged-in or logged-out screen. */
export async function getIntegrator(serverUrl: string, slug: string): Promise<Integrator> {
  const w = await request<IntegratorWire>(serverUrl, `/integrations/${slug}`)
  return {
    id: w.id,
    slug: w.slug,
    name: w.name,
    ownerName: w.owner_name,
    registeredAt: w.registered_at,
    status: w.status,
    category: w.category,
    requestedCapabilities: w.requested_capabilities,
  }
}

export interface IntegratorSummary {
  id: string
  slug: string
  name: string
  ownerName: string
  registeredAt: string
  status: string
  category: IntegratorCategory
}
interface IntegratorSummaryWire {
  id: string
  slug: string
  name: string
  owner_name: string
  registered_at: string
  status: string
  category: IntegratorCategory
}

export interface IntegratorsPage {
  integrators: IntegratorSummary[]
  // Present (non-null) only when another page exists — pass back as
  // `cursor=` to fetch it.
  nextCursor: string | null
}
interface IntegratorsPageWire {
  integrators: IntegratorSummaryWire[]
  next_cursor: string | null
}

/** `GET /integrations{queryString}` — `queryString` passed through as-is
 * (including its leading `?`), same convention as
 * `AccountSession.discoverGuilds`. Accepts `q`/`sort`/`limit`/`cursor`
 * server-side. */
export async function listIntegrators(serverUrl: string, queryString = ''): Promise<IntegratorsPage> {
  const w = await request<IntegratorsPageWire>(serverUrl, `/integrations${queryString}`)
  if (!Array.isArray(w?.integrators)) return w as unknown as IntegratorsPage
  return {
    integrators: w.integrators.map((i) => ({
      id: i.id,
      slug: i.slug,
      name: i.name,
      ownerName: i.owner_name,
      registeredAt: i.registered_at,
      status: i.status,
      category: i.category,
    })),
    nextCursor: w.next_cursor,
  }
}

export interface RegistryMetric {
  value: number
  definition: string
  class: string
}

/** `GET /integrations/{slug}/registry`'s response — never
 * rendered as a bare `value` anywhere downstream. */
export interface IntegratorRegistry {
  players: RegistryMetric
  totalPlayersEver: RegistryMetric
  achievementsIssued: RegistryMetric
  achievementsRevoked: RegistryMetric
  uniqueAchievementHolders: RegistryMetric
}
interface IntegratorRegistryWire {
  players: RegistryMetric
  total_players_ever: RegistryMetric
  achievements_issued: RegistryMetric
  achievements_revoked: RegistryMetric
  unique_achievement_holders: RegistryMetric
}

export async function getIntegratorRegistry(serverUrl: string, slug: string): Promise<IntegratorRegistry> {
  const w = await request<IntegratorRegistryWire>(serverUrl, `/integrations/${slug}/registry`)
  return {
    players: w.players,
    totalPlayersEver: w.total_players_ever,
    achievementsIssued: w.achievements_issued,
    achievementsRevoked: w.achievements_revoked,
    uniqueAchievementHolders: w.unique_achievement_holders,
  }
}

export interface IssuerKey {
  keyId: string
  algorithm: string
  role: 'root' | 'operational'
  validFrom: string
  validUntil: string | null
  revokedAt: string | null
}
interface IssuerKeyWire {
  key_id: string
  algorithm: string
  role: 'root' | 'operational'
  valid_from: string
  valid_until: string | null
  revoked_at: string | null
}

/** `GET /integrations/{slug}/keys` — an issuer's full key
 * history, oldest first, root and operational, valid and revoked. Same
 * public/unauthenticated visibility as `getIntegrator`/`listIntegrators`. */
export async function listIssuerKeys(serverUrl: string, slug: string): Promise<IssuerKey[]> {
  const w = await request<IssuerKeyWire[]>(serverUrl, `/integrations/${slug}/keys`)
  if (!Array.isArray(w)) return w as unknown as IssuerKey[]
  return w.map((k) => ({
    keyId: k.key_id,
    algorithm: k.algorithm,
    role: k.role,
    validFrom: k.valid_from,
    validUntil: k.valid_until,
    revokedAt: k.revoked_at,
  }))
}

export interface AttestationDetail {
  id: string
  issuer: string
  subject: string
  achievement: string
  issuedAt: string
  proof: { keyId: string; algorithm: string }
  authenticity: components['schemas']['AuthenticityResponse']
  validity: components['schemas']['ValidityResponse']
  history: { event: string; at: string; reasonCode?: string; reason?: string }[]
}
type AttestationDetailWire = components['schemas']['AttestationReadResponse']

/** `GET /attestations/{id}` — public, unauthenticated: an
 * attestation's full read shape, including its authenticity/validity
 * verdicts and revocation history. Never a `recognition` field —
 * recognition is computed by the reading integrator against its own
 * trust policy, never by the server. */
export async function getAttestation(serverUrl: string, id: string): Promise<AttestationDetail> {
  const w = await request<AttestationDetailWire>(serverUrl, `/attestations/${id}`)
  return {
    id: w.id,
    issuer: w.issuer,
    subject: w.subject,
    achievement: w.achievement,
    issuedAt: w.issued_at,
    proof: { keyId: w.proof.key_id, algorithm: w.proof.algorithm },
    authenticity: w.authenticity,
    validity: w.validity,
    history: w.history.map((h) => ({ event: h.event, at: h.at, reasonCode: h.reason_code ?? undefined, reason: h.reason ?? undefined })),
  }
}

export interface SchemaVersion {
  id: string
  integratorId: string
  version: number
  protoSource: string
  publishedAt: string
  supersededBy?: string
  defaultVisibility: string
  fieldVisibility: Record<string, string>
}
type SchemaVersionWire = components['schemas']['IntegratorSchemaVersionResponse']
function schemaVersionFromWire(w: SchemaVersionWire): SchemaVersion {
  return {
    id: w.id,
    integratorId: w.integrator_id,
    version: w.version,
    protoSource: w.proto_source,
    publishedAt: w.published_at,
    supersededBy: w.superseded_by ?? undefined,
    defaultVisibility: w.default_visibility,
    fieldVisibility: w.field_visibility,
  }
}

/** `GET /integrations/{slug}/schemas` — every published Integrator
 * Space schema version for this integrator, oldest first. Public,
 * unauthenticated. Empty for an integrator that has never published. */
export async function listSchemaVersions(serverUrl: string, slug: string): Promise<SchemaVersion[]> {
  const w = await request<SchemaVersionWire[]>(serverUrl, `/integrations/${slug}/schemas`)
  if (!Array.isArray(w)) return w as unknown as SchemaVersion[]
  return w.map(schemaVersionFromWire)
}

/** `GET /integrations/{slug}/schemas/{version}` — one published version,
 * verbatim. Public, unauthenticated. */
export async function getSchemaVersion(serverUrl: string, slug: string, version: number): Promise<SchemaVersion> {
  const w = await request<SchemaVersionWire>(serverUrl, `/integrations/${slug}/schemas/${version}`)
  return schemaVersionFromWire(w)
}

export interface SchemaMapping {
  id: string
  integratorId: string
  fromSchemaId: string
  toSchemaId: string
  description: string
  fieldCorrespondence: Record<string, string>
  publishedAt: string
}
type SchemaMappingWire = components['schemas']['IntegratorSchemaMappingResponse']
function schemaMappingFromWire(w: SchemaMappingWire): SchemaMapping {
  return {
    id: w.id,
    integratorId: w.integrator_id,
    fromSchemaId: w.from_schema_id,
    toSchemaId: w.to_schema_id,
    description: w.description,
    fieldCorrespondence: w.field_correspondence,
    publishedAt: w.published_at,
  }
}

/** `GET /integrations/{slug}/mappings` — every published
 * schema-to-schema mapping for this integrator, oldest first. Public,
 * unauthenticated. Documents a correspondence only — never executed or
 * interpreted by this SDK. */
export async function listMappings(serverUrl: string, slug: string): Promise<SchemaMapping[]> {
  const w = await request<SchemaMappingWire[]>(serverUrl, `/integrations/${slug}/mappings`)
  if (!Array.isArray(w)) return w as unknown as SchemaMapping[]
  return w.map(schemaMappingFromWire)
}

/** `GET /integrations/{slug}/mappings/{seq}` — one published mapping,
 * verbatim. Public, unauthenticated. */
export async function getMapping(serverUrl: string, slug: string, seq: number): Promise<SchemaMapping> {
  const w = await request<SchemaMappingWire>(serverUrl, `/integrations/${slug}/mappings/${seq}`)
  return schemaMappingFromWire(w)
}

export interface Recognition {
  recognizerSlug: string
  recognizedSlug: string
  scope: string[]
  publishedAt: string
}
type RecognitionWire = components['schemas']['RecognitionResponse']
function recognitionFromWire(w: RecognitionWire): Recognition {
  return {
    recognizerSlug: w.recognizer_slug,
    recognizedSlug: w.recognized_slug,
    scope: w.scope,
    publishedAt: w.published_at,
  }
}

/** `GET /integrations/{slug}/recognitions` — every integrator `slug`
 * currently, actively recognizes. Public, unauthenticated. */
export async function listRecognitions(serverUrl: string, slug: string): Promise<Recognition[]> {
  const w = await request<RecognitionWire[]>(serverUrl, `/integrations/${slug}/recognitions`)
  if (!Array.isArray(w)) return w as unknown as Recognition[]
  return w.map(recognitionFromWire)
}

/** `GET /integrations/{slug}/recognized-by` — every integrator that
 * currently, actively recognizes `slug`. Public, unauthenticated. */
export async function listRecognizedBy(serverUrl: string, slug: string): Promise<Recognition[]> {
  const w = await request<RecognitionWire[]>(serverUrl, `/integrations/${slug}/recognized-by`)
  if (!Array.isArray(w)) return w as unknown as Recognition[]
  return w.map(recognitionFromWire)
}
