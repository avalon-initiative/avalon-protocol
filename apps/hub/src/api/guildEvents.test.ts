import { describe, expect, it } from 'vitest'
import {
  EVENT_DESCRIPTION_MAX_CHARS,
  EVENT_TITLE_MAX_CHARS,
  rsvpStatusLabel,
  sortByStartsAt,
  splitUpcoming,
  totalRsvps,
  validateEventForm,
} from './guildEvents'
import type { EventResponse } from './types'

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
