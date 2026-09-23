// Pure helpers for the integrator directory — no fetch/token
// awareness here, same "logic stays out of client.ts" split
// apps/hub/src/api/guilds.ts already established for guilds.
import type { IntegratorRegistry, RegistryMetric } from '@avalon-initiative/protocol-sdk'

// Query params for GET /integrations — mirrors
// crates/server/src/integrations.rs::ListIntegratorsQuery. Purely a
// request shape, not a wire response type, so it lives here rather than
// in bindings/ts (whose own listIntegrators just takes the built query
// string), same split apps/hub/src/api/guilds.ts's DiscoverGuildsParams
// already establishes for guild discovery.
export interface ListIntegratorsParams {
  q?: string
  sort?: 'newest' | 'name'
  limit?: number
  cursor?: string
}

// Builds the `?q=&sort=&limit=&cursor=` query string for GET /integrations from a
// params object — mirrors
// apps/hub/src/api/guilds.ts::buildDiscoverQueryString exactly, including
// its "omit rather than send empty/undefined" convention, matching
// crates/server/src/integrations.rs::ListIntegratorsQuery's "omitted means use the
// default" semantics.
export function buildIntegratorsListQueryString(params: ListIntegratorsParams): string {
  const search = new URLSearchParams()
  if (params.q && params.q.trim()) {
    search.set('q', params.q.trim())
  }
  if (params.sort) {
    search.set('sort', params.sort)
  }
  if (params.limit !== undefined) {
    search.set('limit', String(params.limit))
  }
  if (params.cursor) {
    search.set('cursor', params.cursor)
  }
  const query = search.toString()
  return query ? `?${query}` : ''
}

// Mirrors crates/server/src/integrations.rs's own posture: only "active" (today
// the only status IntegratorStatus can ever produce, per its own doc comment) is
// the unmarked, default-styled state — anything else renders as a visibly
// distinct badge rather than reading the same as active. Pure and
// unit-testable independent of any fetch.
export function isActiveIntegratorStatus(status: string): boolean {
  return status === 'active'
}

// Every IntegratorRegistryResponse field as a flat, labeled list — the single
// place the five metric keys/labels are enumerated, so IntegrationProfile.vue
// renders them via a v-for over AvalonMetricTile rather than five
// hand-written copies (and so a component test can assert "every metric
// has a label" without duplicating this list itself). Order matches
// docs/projects/backend-server/architecture/registry.md's metric table.
export interface LabeledMetric extends RegistryMetric {
  key: keyof IntegratorRegistry
  label: string
}

const METRIC_LABELS: Record<keyof IntegratorRegistry, string> = {
  players: 'Players',
  totalPlayersEver: 'Total players ever',
  achievementsIssued: 'Achievements issued',
  achievementsRevoked: 'Achievements revoked',
  uniqueAchievementHolders: 'Unique achievement holders',
}

export function listRegistryMetrics(registry: IntegratorRegistry): LabeledMetric[] {
  return (Object.keys(METRIC_LABELS) as (keyof IntegratorRegistry)[]).map((key) => ({
    key,
    label: METRIC_LABELS[key],
    ...registry[key],
  }))
}
