// Pure guild-events logic (issue #169), kept separate from
// apps/hub/src/api/guilds.ts and guildChat.ts since it's about calendar
// sorting/formatting/RSVP-state, not roster/role merging or message
// composition — following the same split guildChat.ts already documents.
import type { AvalonRsvpRosterGroup } from '@avalon/ui'
import type { EventResponse, RsvpRosterEntry, RsvpStatusValue } from './types'

// Matches crates/server/src/guild_events.rs::EVENT_TITLE_MAX_CHARS exactly
// — surfaced here so a create/edit form can validate client-side before
// ever hitting the server.
export const EVENT_TITLE_MAX_CHARS = 200
export const EVENT_DESCRIPTION_MAX_CHARS = 4000

export interface EventFormValidation {
  valid: boolean
  titleError: string | null
  timeRangeError: string | null
}

// Pure, unit-testable mirror of the server's own validate_title /
// validate_description / validate_time_range.
export function validateEventForm(input: {
  title: string
  description?: string | null
  startsAt: string
  endsAt?: string | null
}): EventFormValidation {
  const trimmedTitle = input.title.trim()
  const descriptionTooLong = (input.description?.length ?? 0) > EVENT_DESCRIPTION_MAX_CHARS

  let titleError: string | null = null
  if (trimmedTitle.length === 0) {
    titleError = 'Title is required.'
  } else if (trimmedTitle.length > EVENT_TITLE_MAX_CHARS) {
    titleError = `Title must be ${EVENT_TITLE_MAX_CHARS} characters or fewer.`
  } else if (descriptionTooLong) {
    titleError = `Description must be ${EVENT_DESCRIPTION_MAX_CHARS} characters or fewer.`
  }

  let timeRangeError: string | null = null
  if (input.endsAt) {
    const starts = Date.parse(input.startsAt)
    const ends = Date.parse(input.endsAt)
    if (!Number.isNaN(starts) && !Number.isNaN(ends) && ends < starts) {
      timeRangeError = 'End time cannot be before the start time.'
    }
  }

  return {
    valid: !titleError && !descriptionTooLong && !timeRangeError,
    titleError,
    timeRangeError,
  }
}

// Calendar views want soonest-first. The server already returns events
// ordered by starts_at (GET .../events), but this is kept as an explicit,
// pure, independently-testable sort rather than trusting response order —
// mirrors toOldestFirst's role in guildChat.ts.
export function sortByStartsAt(events: EventResponse[]): EventResponse[] {
  return [...events].sort((a, b) => Date.parse(a.starts_at) - Date.parse(b.starts_at))
}

// "YYYY-MM-DD" in the *viewer's local* timezone — starts_at is stored/
// transmitted as UTC (same "store universal, translate for UI" model
// AvalonEventCard's display already uses), so grouping events onto a
// calendar grid by day has to localize first or a late-night UTC event
// could land on the wrong day for a reader west of UTC.
export function localDateKey(isoUtc: string): string {
  const d = new Date(isoUtc)
  const y = d.getFullYear()
  const m = String(d.getMonth() + 1).padStart(2, '0')
  const day = String(d.getDate()).padStart(2, '0')
  return `${y}-${m}-${day}`
}

// Splits a list into "still upcoming" vs. "already started/ended", given
// the current time — used to fade or section past events in the calendar
// view without a second server round-trip.
export function splitUpcoming(
  events: EventResponse[],
  now: Date = new Date(),
): { upcoming: EventResponse[]; past: EventResponse[] } {
  const cutoff = now.getTime()
  const upcoming: EventResponse[] = []
  const past: EventResponse[] = []
  for (const event of events) {
    const end = event.ends_at ? Date.parse(event.ends_at) : Date.parse(event.starts_at)
    if (end >= cutoff) {
      upcoming.push(event)
    } else {
      past.push(event)
    }
  }
  return { upcoming, past }
}

export function totalRsvps(event: EventResponse): number {
  return event.rsvp_counts.going + event.rsvp_counts.maybe + event.rsvp_counts.not_going
}

// The three status values in a fixed, presentation-friendly order — same
// "going, maybe, not_going" order the server's RsvpCounts struct uses.
export const RSVP_STATUS_ORDER: RsvpStatusValue[] = ['going', 'maybe', 'not_going']

export function rsvpStatusLabel(status: RsvpStatusValue): string {
  switch (status) {
    case 'going':
      return 'Going'
    case 'maybe':
      return 'Maybe'
    case 'not_going':
      return "Can't go"
  }
}

// Per-member RSVP roster (issue #248). Groups raw guild_event_rsvps rows
// (GET .../events/{eid}/rsvps) into going/maybe/not_going buckets of
// resolved display names, in RSVP_STATUS_ORDER — pure and independently
// testable, same split useGuildChat's resolveAuthorNames/authorNames map
// keeps between fetch and render. An identity id with no resolved name yet
// falls back to the raw id rather than being dropped, matching how chat
// authors degrade when a profile lookup hasn't landed.
export function groupRsvpRoster(
  entries: RsvpRosterEntry[],
  namesById: Record<string, string>,
): AvalonRsvpRosterGroup[] {
  const buckets: Record<RsvpStatusValue, string[]> = { going: [], maybe: [], not_going: [] }
  for (const entry of entries) {
    buckets[entry.status].push(namesById[entry.identity_id] ?? entry.identity_id)
  }
  return RSVP_STATUS_ORDER.map((status) => ({
    status,
    label: rsvpStatusLabel(status),
    names: buckets[status],
  }))
}
