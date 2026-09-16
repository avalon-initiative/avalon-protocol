export interface AvalonEventCardRsvpCounts {
  going: number
  maybe: number
  not_going: number
}

export interface AvalonEventCardProps {
  title: string
  description?: string
  // ISO 8601 timestamps — this component only formats/displays them, it
  // never parses or validates (see apps/hub/src/api/guildEvents.ts for the
  // pure logic that does).
  startsAt: string
  endsAt?: string
  rsvpCounts: AvalonEventCardRsvpCounts
  // Issue #458. Defaults to `true` — every existing caller keeps working
  // unchanged. `false` means the caller has `view` but not `view_details`
  // on this event: `description`/`rsvpCounts` are placeholder values, not
  // real data, so this renders a "details hidden" hint instead of
  // pretending they're meaningful.
  detailsVisible?: boolean
}
