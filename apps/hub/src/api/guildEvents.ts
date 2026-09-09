// Pure guild-events logic (issue #169), kept separate from
// apps/hub/src/api/guilds.ts and guildChat.ts since it's about calendar
// sorting/formatting/RSVP-state, not roster/role merging or message
// composition — following the same split guildChat.ts already documents.
import type { EventResponse, RsvpStatusValue } from './types'

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
