export interface AvalonGuildCardProps {
  name: string
  tag: string
  description?: string
  memberCount: number
  // Issue #154's discovery board is the first caller that has a
  // `recruiting` flag on hand — omitted (not `false`) everywhere else
  // (e.g. Guilds.vue's "my guilds" list), so the badge is opt-in per call
  // site rather than always rendering "not recruiting" noise for guilds
  // that were never advertising in the first place.
  recruiting?: boolean
  // Issue #246: a small badge image, distinct from a guild's wider banner —
  // omitted (not rendered) when a guild has no icon set, same "opt-in
  // per call site" posture `recruiting` already takes.
  iconUrl?: string
  // Issue #258: the guild's wider cover image (same field `Guild.vue`'s
  // page header already renders as `guild.banner`) — omitted (not
  // rendered) when a guild has no banner set, same opt-in posture as
  // `iconUrl`.
  bannerUrl?: string
}
