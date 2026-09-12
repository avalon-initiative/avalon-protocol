// Issue #332: the small, fixed built-in icon set a definition falls back
// to when it has no integrator-hosted `iconUrl` — generic enough to cover
// games/apps/services alike. A subset of AvalonIcon's own AvalonIconName
// (packages/ui/src/components/AvalonIcon.types.ts), kept as its own
// narrower union here since not every nav/status icon makes sense as an
// achievement's visual identity.
export type AchievementIconName = 'trophy' | 'star' | 'shield' | 'sword'

// One entry in an attestation's history (#81/#85's revocation model) —
// "issued" is always present, "revoked" appended once a revocation exists.
// No "superseded"/"reinstated" event kind exists in the protocol yet (see
// docs/architecture/revocation.md), so this stays an open string rather
// than a closed union that would need updating the moment one is added.
export interface AvalonAchievementHistoryEntry {
  event: string
  at: string
  reasonCode?: string
  reason?: string
}

// One claim on the achievements view (#35). Issuer name/slug come in
// separate so this card can render a provenance chip without owning
// routing itself — same "props in, event out, caller owns navigation"
// split AvalonGameCard/AvalonGuildCard already use.
export interface AvalonAchievementCardProps {
  achievementName: string
  // Both optional (#332): `iconUrl`, when present, always wins over
  // `icon` — never a silent fallback to the built-in icon just because
  // both happen to be set. Absent/undefined `icon` with no `iconUrl`
  // renders the 'trophy' default so a claim never shows a blank slot.
  icon?: AchievementIconName
  iconUrl?: string
  issuerName: string
  issuerSlug: string
  issuedAt: string
  // Mirrors avalon_protocol::achievements::Validity's two cases
  // (crates/server/src/attestations.rs's ValidityResponse) — a claim is
  // never scored or ranked here, only ever valid/invalid, each a fact with
  // its own reason (ADR #77: the Hub renders verification results, it
  // never computes trust or rank).
  status: 'valid' | 'invalid'
  invalidReason?: string
  // Always at least one entry ("issued"). A revoked claim keeps its
  // issuance entry too — revocation is history, never an empty slot.
  history: AvalonAchievementHistoryEntry[]
}
