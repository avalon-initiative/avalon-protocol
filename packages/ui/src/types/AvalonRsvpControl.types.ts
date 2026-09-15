export type AvalonRsvpStatus = 'going' | 'maybe' | 'not_going'

export interface AvalonRsvpControlProps {
  // The viewer's own current RSVP, or `undefined` if they haven't
  // responded yet. Never another member's — this component only ever
  // represents/mutates the caller's own row
  // (`crates/server/src/guild_events.rs::upsert_rsvp` is self-service
  // only).
  currentStatus?: AvalonRsvpStatus
  disabled?: boolean
}
