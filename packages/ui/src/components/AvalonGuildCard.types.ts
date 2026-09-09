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
}
