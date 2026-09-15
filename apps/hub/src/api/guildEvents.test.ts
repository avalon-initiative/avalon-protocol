import { describe, expect, it } from 'vitest'
import {
  EVENT_DESCRIPTION_MAX_CHARS,
  EVENT_TITLE_MAX_CHARS,
  groupRsvpRoster,
  localDateKey,
  rsvpStatusLabel,
  sortByStartsAt,
  splitUpcoming,
  totalRsvps,
  validateEventForm,
} from './guildEvents'
import type { EventResponse, RsvpRosterEntry } from './types'

function makeEvent(overrides: Partial<EventResponse> = {}): EventResponse {
  return {
    id: 'e1',
    guild_id: 'g1',
    channel_id: null,
    title: 'Raid night',
    description: null,
    starts_at: '2026-09-10T20:00:00Z',
    ends_at: null,
    created_by: 'u1',
    created_at: '2026-09-01T00:00:00Z',
    rsvp_counts: { going: 0, maybe: 0, not_going: 0 },
    public: false,
    ...overrides,
  }
}

describe('validateEventForm', () => {
  it('rejects an empty title', () => {
    const result = validateEventForm({ title: '   ', startsAt: '2026-09-10T20:00:00Z' })
    expect(result.valid).toBe(false)
    expect(result.titleError).toBe('Title is required.')
  })

  it('rejects a title over the length cap', () => {
    const result = validateEventForm({
      title: 'a'.repeat(EVENT_TITLE_MAX_CHARS + 1),
      startsAt: '2026-09-10T20:00:00Z',
    })
    expect(result.valid).toBe(false)
  })

  it('accepts a title exactly at the cap', () => {
    const result = validateEventForm({
      title: 'a'.repeat(EVENT_TITLE_MAX_CHARS),
      startsAt: '2026-09-10T20:00:00Z',
    })
    expect(result.valid).toBe(true)
  })

  it('rejects an overlong description', () => {
    const result = validateEventForm({
      title: 'Raid night',
      description: 'a'.repeat(EVENT_DESCRIPTION_MAX_CHARS + 1),
      startsAt: '2026-09-10T20:00:00Z',
    })
    expect(result.valid).toBe(false)
  })

  it('rejects ends_at before starts_at', () => {
    const result = validateEventForm({
      title: 'Raid night',
      startsAt: '2026-09-10T20:00:00Z',
      endsAt: '2026-09-10T19:00:00Z',
    })
    expect(result.valid).toBe(false)
    expect(result.timeRangeError).toBe('End time cannot be before the start time.')
  })

  it('accepts a well-formed event with no end time', () => {
    const result = validateEventForm({ title: 'Raid night', startsAt: '2026-09-10T20:00:00Z' })
    expect(result.valid).toBe(true)
    expect(result.titleError).toBeNull()
    expect(result.timeRangeError).toBeNull()
  })

  it('accepts ends_at equal to starts_at', () => {
    const result = validateEventForm({
      title: 'Raid night',
      startsAt: '2026-09-10T20:00:00Z',
      endsAt: '2026-09-10T20:00:00Z',
    })
    expect(result.valid).toBe(true)
  })
})

describe('sortByStartsAt', () => {
  it('sorts soonest first', () => {
    const events = [
      makeEvent({ id: 'later', starts_at: '2026-09-12T00:00:00Z' }),
      makeEvent({ id: 'sooner', starts_at: '2026-09-10T00:00:00Z' }),
    ]
    expect(sortByStartsAt(events).map((e) => e.id)).toEqual(['sooner', 'later'])
  })

  it('does not mutate the input array', () => {
    const events = [
      makeEvent({ id: 'later', starts_at: '2026-09-12T00:00:00Z' }),
      makeEvent({ id: 'sooner', starts_at: '2026-09-10T00:00:00Z' }),
    ]
    sortByStartsAt(events)
    expect(events.map((e) => e.id)).toEqual(['later', 'sooner'])
  })
})

describe('splitUpcoming', () => {
  const now = new Date('2026-09-10T12:00:00Z')

  it('buckets by ends_at when present, falling back to starts_at', () => {
    const upcoming = makeEvent({ id: 'upcoming', starts_at: '2026-09-11T00:00:00Z' })
    const past = makeEvent({ id: 'past', starts_at: '2026-09-01T00:00:00Z' })
    const endedButStartedBefore = makeEvent({
      id: 'still-going',
      starts_at: '2026-09-10T10:00:00Z',
      ends_at: '2026-09-10T13:00:00Z',
    })
    const result = splitUpcoming([upcoming, past, endedButStartedBefore], now)
    expect(result.upcoming.map((e) => e.id).sort()).toEqual(['still-going', 'upcoming'])
    expect(result.past.map((e) => e.id)).toEqual(['past'])
  })
})

describe('totalRsvps', () => {
  it('sums all three statuses', () => {
    const event = makeEvent({ rsvp_counts: { going: 3, maybe: 1, not_going: 2 } })
    expect(totalRsvps(event)).toBe(6)
  })
})

describe('rsvpStatusLabel', () => {
  it('labels each status', () => {
    expect(rsvpStatusLabel('going')).toBe('Going')
    expect(rsvpStatusLabel('maybe')).toBe('Maybe')
    expect(rsvpStatusLabel('not_going')).toBe("Can't go")
  })
})

describe('localDateKey', () => {
  // Deliberately round-trips through a locally-constructed Date rather
  // than asserting a hardcoded "YYYY-MM-DD" — a fixed UTC string's local
  // calendar day depends on whichever timezone the test runner is in,
  // exactly the class of bug this function exists to get right.
  it('recovers the same local calendar day a Date was built from', () => {
    const local = new Date(2026, 8, 15, 22, 30) // Sep 15 2026, 10:30pm local
    expect(localDateKey(local.toISOString())).toBe('2026-09-15')
  })

  it('pads single-digit months and days', () => {
    const local = new Date(2026, 0, 5, 9, 0) // Jan 5 2026 local
    expect(localDateKey(local.toISOString())).toBe('2026-01-05')
  })
})

describe('groupRsvpRoster', () => {
  function makeEntry(overrides: Partial<RsvpRosterEntry> = {}): RsvpRosterEntry {
    return {
      identity_id: 'id-1',
      status: 'going',
      responded_at: '2026-09-10T20:00:00Z',
      ...overrides,
    }
  }

  it('buckets resolved display names by status, in going/maybe/not_going order', () => {
    const entries = [
      makeEntry({ identity_id: 'id-1', status: 'going' }),
      makeEntry({ identity_id: 'id-2', status: 'maybe' }),
      makeEntry({ identity_id: 'id-3', status: 'not_going' }),
      makeEntry({ identity_id: 'id-4', status: 'going' }),
    ]
    const namesById = {
      'id-1': 'Rowan#1234',
      'id-2': 'Sable#0007',
      'id-3': 'Quill#4821',
      'id-4': 'Ash#9999',
    }
    const groups = groupRsvpRoster(entries, namesById)
    expect(groups.map((g) => g.status)).toEqual(['going', 'maybe', 'not_going'])
    expect(groups.map((g) => g.label)).toEqual(['Going', 'Maybe', "Can't go"])
    expect(groups.find((g) => g.status === 'going')?.names).toEqual(['Rowan#1234', 'Ash#9999'])
    expect(groups.find((g) => g.status === 'maybe')?.names).toEqual(['Sable#0007'])
    expect(groups.find((g) => g.status === 'not_going')?.names).toEqual(['Quill#4821'])
  })

  it('falls back to the raw identity id when no name has resolved yet', () => {
    const entries = [makeEntry({ identity_id: 'unresolved-id', status: 'going' })]
    const groups = groupRsvpRoster(entries, {})
    expect(groups.find((g) => g.status === 'going')?.names).toEqual(['unresolved-id'])
  })

  it('returns all three groups, empty, when there are no RSVPs', () => {
    const groups = groupRsvpRoster([], {})
    expect(groups).toHaveLength(3)
    for (const group of groups) {
      expect(group.names).toEqual([])
    }
  })
})
