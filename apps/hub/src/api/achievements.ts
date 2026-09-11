// Orchestrates the achievements view (issue #35) on top of #34's
// GET /me/achievements: an attestation only ever carries its
// achievement/milestone's GlobalId ref, never a display name or the
// issuer's own name, so both get resolved separately and merged
// client-side — same "fetch separately, merge client-side" pattern
// apps/hub/src/api/friends.ts/guilds.ts already establish for data the
// primary response doesn't embed. Sorting/filtering below is entirely
// client-side presentation, never a protocol event or server computation
// — matching ADR #77's "the Hub renders verification results, it never
// computes trust or rank" rule.
import * as api from './client'
import type { AttestationHistoryEntryResponse, AttestationResponse } from './types'

// `issuer` on the wire is "<namespace>:<slug>" (crates/server/src/achievements.rs's
// `issuer_str`), never a full GlobalId with a trailing kind/verb.
export function parseIssuerSlug(issuer: string): string | null {
  const parts = issuer.split(':')
  return parts.length === 2 ? parts[1] : null
}

// `achievement` on the wire is the definition's full GlobalId ref —
// "<namespace>:<slug>:<achievement|milestone>:<key>"
// (crates/server/src/achievements.rs::definition_ref). `namespace` says
// which listing endpoint carries this definition's name: `game` ->
// GET /games/{slug}/achievements, `app`/`service` ->
// GET /integrations/{slug}/milestones.
export function parseAchievementRef(
  ref: string,
): { namespace: string; slug: string; key: string } | null {
  const parts = ref.split(':')
  return parts.length === 4 ? { namespace: parts[0], slug: parts[1], key: parts[3] } : null
}

export interface AchievementHistoryEntry {
  event: string
  at: string
  reasonCode?: string
  reason?: string
}

function mergeHistoryEntry(entry: AttestationHistoryEntryResponse): AchievementHistoryEntry {
  return { event: entry.event, at: entry.at, reasonCode: entry.reason_code, reason: entry.reason }
}

export interface Achievement {
  id: string
  achievementRef: string
  // Undefined only if the definition itself couldn't be resolved (e.g. the
  // lookup failed) — the UI falls back to the ref's bare key rather than
  // assuming a name exists.
  achievementName?: string
  issuerSlug: string
  // Undefined only if the issuer's own profile couldn't be resolved (a
  // deleted/unreachable issuer, or the lookup itself failed) — the UI
  // falls back to the bare slug rather than assuming a name exists.
  issuerName?: string
  issuedAt: string
  status: 'valid' | 'invalid'
  invalidReason?: string
  history: AchievementHistoryEntry[]
}

// Pure merge, testable without any network call — mirrors
// apps/hub/src/api/friends.ts's mergeFriend shape.
export function mergeAchievement(
  attestation: AttestationResponse,
  issuerNameBySlug: Map<string, string> = new Map(),
  achievementNameByRef: Map<string, string> = new Map(),
): Achievement {
  const issuerSlug = parseIssuerSlug(attestation.issuer) ?? attestation.issuer
  return {
    id: attestation.id,
    achievementRef: attestation.achievement,
    achievementName: achievementNameByRef.get(attestation.achievement),
    issuerSlug,
    issuerName: issuerNameBySlug.get(issuerSlug),
    issuedAt: attestation.issued_at,
    status: attestation.validity.status,
    invalidReason: attestation.validity.reason,
    history: attestation.history.map(mergeHistoryEntry),
  }
}

export async function listMyAchievements(token: string): Promise<Achievement[]> {
  const attestations = await api.getMyAchievements(token)
  if (attestations.length === 0) {
    return []
  }

  const issuerSlugs = [...new Set(attestations.map((a) => parseIssuerSlug(a.issuer) ?? a.issuer))]
  const gamesLookup = Promise.all(issuerSlugs.map((slug) => api.getGamePublic(slug).catch(() => null)))

  // One definitions-list call per distinct (namespace, slug) pair rather
  // than per attestation — a player with many claims from the same issuer
  // shouldn't refetch that issuer's whole definition list once per claim.
  const refs = attestations.map((a) => parseAchievementRef(a.achievement)).filter((r) => r !== null)
  const issuerKey = (r: { namespace: string; slug: string }) => `${r.namespace}:${r.slug}`
  const uniqueIssuers = new Map(refs.map((r) => [issuerKey(r), r]))
  const definitionsLookup = Promise.all(
    [...uniqueIssuers.values()].map((r) =>
      (r.namespace === 'game'
        ? api.listAchievementDefinitions(r.slug)
        : api.listMilestoneDefinitions(r.slug)
      ).catch(() => []),
    ),
  )

  const [games, definitionLists] = await Promise.all([gamesLookup, definitionsLookup])

  const issuerNameBySlug = new Map<string, string>()
  games.forEach((game, index) => {
    if (game) issuerNameBySlug.set(issuerSlugs[index], game.name)
  })

  const achievementNameByRef = new Map<string, string>()
  definitionLists.flat().forEach((def) => achievementNameByRef.set(def.id, def.name))

  return attestations.map((a) => mergeAchievement(a, issuerNameBySlug, achievementNameByRef))
}

export type AchievementSort = 'date' | 'name' | 'game'

// Pure, unit-testable sort — the view binds this to a dropdown. Ties break
// on the achievement ref for stability rather than leaving equal-key rows
// in an arbitrary/fetch order.
export function sortAchievements(achievements: Achievement[], sort: AchievementSort): Achievement[] {
  const sorted = [...achievements]
  const name = (a: Achievement) => a.achievementName ?? a.achievementRef
  switch (sort) {
    case 'date':
      sorted.sort((a, b) => b.issuedAt.localeCompare(a.issuedAt))
      break
    case 'name':
      sorted.sort((a, b) => name(a).localeCompare(name(b)))
      break
    case 'game':
      sorted.sort(
        (a, b) =>
          (a.issuerName ?? a.issuerSlug).localeCompare(b.issuerName ?? b.issuerSlug) ||
          name(a).localeCompare(name(b)),
      )
      break
  }
  return sorted
}

// Pure filter-by-issuer-slug — `null`/omitted means "every game".
export function filterAchievementsByGame(
  achievements: Achievement[],
  issuerSlug: string | null,
): Achievement[] {
  if (!issuerSlug) return achievements
  return achievements.filter((a) => a.issuerSlug === issuerSlug)
}
