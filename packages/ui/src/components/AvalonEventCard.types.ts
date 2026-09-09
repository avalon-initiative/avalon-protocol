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
}
