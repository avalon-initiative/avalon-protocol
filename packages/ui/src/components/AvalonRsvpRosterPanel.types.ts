export type AvalonRsvpRosterStatus = 'going' | 'maybe' | 'not_going'

// One status bucket of already-resolved display names — the caller (Hub)
// owns fetching guild_event_rsvps rows and resolving identity ids to
// "display_name#discriminator" via GET /identities/profiles; this
// component only renders the result, same "props in, click out" split
// AvalonEventCard/AvalonRsvpControl already use.
export interface AvalonRsvpRosterGroup {
  status: AvalonRsvpRosterStatus
  label: string
  names: string[]
}

export interface AvalonRsvpRosterPanelProps {
  open: boolean
  eventTitle: string
  loading?: boolean
  error?: string
  groups: AvalonRsvpRosterGroup[]
}
