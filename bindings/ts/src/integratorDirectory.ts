// Public, unauthenticated integrator-directory/registry reads (issue
// #270/#261/#90/#31/#324/#325) — free-standing functions, not session
// methods, same convention as ledger.ts's getLatestSth. See
// crates/server/src/integrations.rs/registry.rs/achievements.rs.
import { request } from './http.js'

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
  return w.map(achievementDefinitionFromWire)
}

/** `GET /integrations/{slug}/milestones`. */
export async function listMilestoneDefinitions(serverUrl: string, slug: string): Promise<AchievementDefinition[]> {
  const w = await request<AchievementDefinitionWire[]>(serverUrl, `/integrations/${slug}/milestones`)
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

/** `GET /integrations/{slug}/registry`'s response (issue #261) — never
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

/** `GET /integrations/{slug}/keys` (issue #90) — an issuer's full key
 * history, oldest first, root and operational, valid and revoked. Same
 * public/unauthenticated visibility as `getIntegrator`/`listIntegrators`. */
export async function listIssuerKeys(serverUrl: string, slug: string): Promise<IssuerKey[]> {
  const w = await request<IssuerKeyWire[]>(serverUrl, `/integrations/${slug}/keys`)
  return w.map((k) => ({
    keyId: k.key_id,
    algorithm: k.algorithm,
    role: k.role,
    validFrom: k.valid_from,
    validUntil: k.valid_until,
    revokedAt: k.revoked_at,
  }))
}
