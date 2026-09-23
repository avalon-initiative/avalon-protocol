// Pure guild-events logic, kept separate from
// apps/hub/src/api/guilds.ts and guildChat.ts since it's about calendar
// sorting/formatting/RSVP-state, not roster/role merging or message
// composition — following the same split guildChat.ts already documents.
import type { AvalonRsvpRosterGroup } from '@avalon/ui'
import type { GuildEvent, RsvpRosterEntry, RsvpStatus } from '@avalon-initiative/protocol-sdk'

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
export function sortByStartsAt(events: GuildEvent[]): GuildEvent[] {
  return [...events].sort((a, b) => Date.parse(a.startsAt) - Date.parse(b.startsAt))
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

// "YYYY-MM-DDTHH:mm" in the viewer's local timezone — the exact format
// AvalonDateTimeField's modelValue expects, and the inverse of
// onCreateEvent/onSaveEditEvent's `new Date(value).toISOString()`. Used to
// pre-fill the edit-event form from a server response's UTC starts_at/
// ends_at, same "store universal, translate for UI" model localDateKey
// above already documents.
export function toLocalDateTimeInput(isoUtc: string): string {
  const d = new Date(isoUtc)
  const y = d.getFullYear()
  const m = String(d.getMonth() + 1).padStart(2, '0')
  const day = String(d.getDate()).padStart(2, '0')
  const h = String(d.getHours()).padStart(2, '0')
  const min = String(d.getMinutes()).padStart(2, '0')
  return `${y}-${m}-${day}T${h}:${min}`
}

// Splits a list into "still upcoming" vs. "already started/ended", given
// the current time — used to fade or section past events in the calendar
// view without a second server round-trip.
export function splitUpcoming(
  events: GuildEvent[],
  now: Date = new Date(),
): { upcoming: GuildEvent[]; past: GuildEvent[] } {
  const cutoff = now.getTime()
  const upcoming: GuildEvent[] = []
  const past: GuildEvent[] = []
  for (const event of events) {
    const end = event.endsAt ? Date.parse(event.endsAt) : Date.parse(event.startsAt)
    if (end >= cutoff) {
      upcoming.push(event)
    } else {
      past.push(event)
    }
  }
  return { upcoming, past }
}

export function totalRsvps(event: GuildEvent): number {
  return event.rsvpCounts.going + event.rsvpCounts.maybe + event.rsvpCounts.notGoing
}

// The three status values in a fixed, presentation-friendly order — same
// "going, maybe, not_going" order the server's RsvpCounts struct uses.
export const RSVP_STATUS_ORDER: RsvpStatus[] = ['going', 'maybe', 'not_going']

export function rsvpStatusLabel(status: RsvpStatus): string {
  switch (status) {
    case 'going':
      return 'Going'
    case 'maybe':
      return 'Maybe'
    case 'not_going':
      return "Can't go"
  }
}

// Per-member RSVP roster. Groups raw guild_event_rsvps rows
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
  const buckets: Record<RsvpStatus, string[]> = { going: [], maybe: [], not_going: [] }
  for (const entry of entries) {
    buckets[entry.status].push(namesById[entry.identityId] ?? entry.identityId)
  }
  return RSVP_STATUS_ORDER.map((status) => ({
    status,
    label: rsvpStatusLabel(status),
    names: buckets[status],
  }))
}
