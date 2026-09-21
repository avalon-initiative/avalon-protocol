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
import type { AchievementIconName } from '@avalon/ui'
import { getIntegrator, listAchievementDefinitions, listMilestoneDefinitions } from '@avalon/sdk'
import type { AccountSession, Attestation, AttestationHistoryEntry as AttestationHistoryEntryWire } from '@avalon/sdk'
import { getServerUrl } from './serverUrl'

// `issuer` on the wire is "<namespace>:<slug>" (crates/server/src/achievements.rs's
// `issuer_str`), never a full GlobalId with a trailing kind/verb.
export function parseIssuerSlug(issuer: string): string | null {
  const parts = issuer.split(':')
  return parts.length === 2 ? parts[1] : null
}

// `achievement` on the wire is the definition's full GlobalId ref —
// "<namespace>:<slug>:<achievement|milestone>:<key>"
// (crates/server/src/achievements.rs::definition_ref). `namespace` says
// which listing endpoint carries this definition's name: `integrator` ->
// GET /integrations/{slug}/achievements, `app`/`service` ->
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

function mergeHistoryEntry(entry: AttestationHistoryEntryWire): AchievementHistoryEntry {
  return { event: entry.event, at: entry.at, reasonCode: entry.reasonCode, reason: entry.reason }
}

// #332's fixed built-in set — kept in sync with packages/ui's own
// AchievementIconName. A definition's `icon` string is only ever trusted
// as one of these; anything else (shouldn't happen, since the server
// validates it, but a stale/foreign client could still send garbage) falls
// back to `undefined` so AvalonAchievementCard's own 'trophy' default
// applies instead of passing through an unrecognized icon name.
const BUILTIN_ICON_NAMES: ReadonlySet<string> = new Set(['trophy', 'star', 'shield', 'sword'])

function asAchievementIconName(icon: string | undefined): AchievementIconName | undefined {
  return icon !== undefined && BUILTIN_ICON_NAMES.has(icon)
    ? (icon as AchievementIconName)
    : undefined
}

export interface Achievement {
  id: string
  achievementRef: string
  // Undefined only if the definition itself couldn't be resolved (e.g. the
  // lookup failed) — the UI falls back to the ref's bare key rather than
  // assuming a name exists.
  achievementName?: string
  // Both undefined only if the definition itself couldn't be resolved —
  // AvalonAchievementCard's own 'trophy' default applies in that case too,
  // same as an unresolved achievementName falls back to the bare ref.
  achievementIcon?: AchievementIconName
  achievementIconUrl?: string
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
export interface AchievementDefinitionSummary {
  name: string
  icon: string
  iconUrl?: string
}

export function mergeAchievement(
  attestation: Attestation,
  issuerNameBySlug: Map<string, string> = new Map(),
  achievementByRef: Map<string, AchievementDefinitionSummary> = new Map(),
): Achievement {
  const issuerSlug = parseIssuerSlug(attestation.issuer) ?? attestation.issuer
  const definition = achievementByRef.get(attestation.achievement)
  return {
    id: attestation.id,
    achievementRef: attestation.achievement,
    achievementName: definition?.name,
    achievementIcon: asAchievementIconName(definition?.icon),
    achievementIconUrl: definition?.iconUrl,
    issuerSlug,
    issuerName: issuerNameBySlug.get(issuerSlug),
    issuedAt: attestation.issuedAt,
    status: attestation.validity.status,
    invalidReason: attestation.validity.reason,
    history: attestation.history.map(mergeHistoryEntry),
  }
}

export async function listMyAchievements(session: AccountSession): Promise<Achievement[]> {
  const attestations = await session.getMyAchievements()
  if (!Array.isArray(attestations) || attestations.length === 0) {
    return []
  }

  const serverUrl = getServerUrl()
  const issuerSlugs = [...new Set(attestations.map((a) => parseIssuerSlug(a.issuer) ?? a.issuer))]
  const integratorsLookup = Promise.all(
    issuerSlugs.map((slug) => getIntegrator(serverUrl, slug).catch(() => null)),
  )

  // One definitions-list call per distinct (namespace, slug) pair rather
  // than per attestation — a user with many claims from the same issuer
  // shouldn't refetch that issuer's whole definition list once per claim.
  const refs = attestations.map((a) => parseAchievementRef(a.achievement)).filter((r) => r !== null)
  const issuerKey = (r: { namespace: string; slug: string }) => `${r.namespace}:${r.slug}`
  const uniqueIssuers = new Map(refs.map((r) => [issuerKey(r), r]))
  const definitionsLookup = Promise.all(
    [...uniqueIssuers.values()].map((r) =>
      (r.namespace === 'game'
        ? listAchievementDefinitions(serverUrl, r.slug)
        : listMilestoneDefinitions(serverUrl, r.slug)
      ).catch(() => []),
    ),
  )

  const [integrators, definitionLists] = await Promise.all([integratorsLookup, definitionsLookup])

  const issuerNameBySlug = new Map<string, string>()
  integrators.forEach((integrator, index) => {
    if (integrator) issuerNameBySlug.set(issuerSlugs[index], integrator.name)
  })

  const achievementByRef = new Map<string, AchievementDefinitionSummary>()
  definitionLists
    .flat()
    .forEach((def) =>
      achievementByRef.set(def.id, { name: def.name, icon: def.icon, iconUrl: def.iconUrl }),
    )

  return attestations.map((a) => mergeAchievement(a, issuerNameBySlug, achievementByRef))
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

// Pure filter-by-issuer-slug — `null`/omitted means "every integrator".
export function filterAchievementsByIntegrator(
  achievements: Achievement[],
  issuerSlug: string | null,
): Achievement[] {
  if (!issuerSlug) return achievements
  return achievements.filter((a) => a.issuerSlug === issuerSlug)
}
