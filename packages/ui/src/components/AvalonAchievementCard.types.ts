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
