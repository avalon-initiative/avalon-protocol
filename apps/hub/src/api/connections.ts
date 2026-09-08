// Plain-language capability descriptions + pure helpers for the game
// connect/consent flow (#27, #83). Mirrors apps/hub/src/api/guilds.ts's
// shape: wire-format merging/lookup logic that's testable without a
// network call, kept out of the view components themselves.
import type { GameBindingResponse } from './types'

// One entry per crates/protocol/src/permissions.rs::Capability::KNOWN wire
// string (Capability::as_str()) — kept in sync by hand since the Hub
// doesn't share Rust enums with the server. An unrecognized capability
// (Capability::Other(_), or a future addition this build predates) still
// renders via the fallback in capabilityDescription below rather than
// crashing the consent view.
export const CAPABILITY_DESCRIPTIONS: Record<string, string> = {
  'identity.read': 'See your identity id',
  'profile.read': 'See your public profile (display name, avatar)',
  'friends.read': 'See your friends list',
  'presence.read': "See your friends' online status",
  'presence.publish': 'Publish your online status to friends',
  'guilds.read': 'See the guilds you belong to',
  'guilds.chat': 'Send messages in your guild channels',
  'guilds.issue': 'Create and manage guild content on your behalf',
  'achievements.read': 'See your achievements',
  'achievements.issue': 'Award you achievements',
  'assets.read': 'See your assets',
  'assets.issue': 'Issue you assets',
  'wallet.read': 'See your wallet balance',
  'wallet.write': 'Spend from your wallet',
}

// Falls back to the raw wire string for anything not in the table above —
// same "never dropped, never rejected" honesty
// crates/protocol/src/permissions.rs's Capability::Other documents, applied
// to display text instead of wire encoding.
export function capabilityDescription(capability: string): string {
  return CAPABILITY_DESCRIPTIONS[capability] ?? capability
}

// A binding with no active grants under it still shows in the connected-
// games list (ending every grant doesn't itself end the binding) — this
// only decides what capabilities are still active for display purposes.
export function activeCapabilities(binding: GameBindingResponse): string[] {
  return binding.grants.map((g) => g.capability)
}
