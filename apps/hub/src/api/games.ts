// Pure helpers for the game directory (issue #270) — no fetch/token
// awareness here, same "logic stays out of client.ts" split
// apps/hub/src/api/guilds.ts already established for guilds.
import type { GameRegistryResponse, ListGamesParams, MetricResponse } from './types'

// Builds the `?q=&sort=&limit=&cursor=` query string for GET /games from a
// params object — mirrors
// apps/hub/src/api/guilds.ts::buildDiscoverQueryString exactly, including
// its "omit rather than send empty/undefined" convention, matching
// crates/server/src/games.rs::ListGamesQuery's "omitted means use the
// default" semantics.
export function buildGamesListQueryString(params: ListGamesParams): string {
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

// Mirrors crates/server/src/games.rs's own posture: only "active" (today
// the only status GameStatus can ever produce, per its own doc comment) is
// the unmarked, default-styled state — anything else renders as a visibly
// distinct badge rather than reading the same as active. Pure and
// unit-testable independent of any fetch.
export function isActiveGameStatus(status: string): boolean {
  return status === 'active'
}

// Every GameRegistryResponse field as a flat, labeled list — the single
// place the five metric keys/labels are enumerated, so GameProfile.vue
// renders them via a v-for over AvalonMetricTile rather than five
// hand-written copies (and so a component test can assert "every metric
// has a label" without duplicating this list itself). Order matches
// docs/architecture/game-registry.md's metric table.
export interface LabeledMetric extends MetricResponse {
  key: keyof GameRegistryResponse
  label: string
}

const METRIC_LABELS: Record<keyof GameRegistryResponse, string> = {
  players: 'Players',
  total_players_ever: 'Total players ever',
  achievements_issued: 'Achievements issued',
  achievements_revoked: 'Achievements revoked',
  unique_achievement_holders: 'Unique achievement holders',
}

export function listRegistryMetrics(registry: GameRegistryResponse): LabeledMetric[] {
  return (Object.keys(METRIC_LABELS) as (keyof GameRegistryResponse)[]).map((key) => ({
    key,
    label: METRIC_LABELS[key],
    ...registry[key],
  }))
}
