import { describe, expect, it } from 'vitest'
import { formatActivityTimestamp, summarizeActivityEntry } from './activityFeed'
import type { HistoryEntry } from '@avalon-initiative/protocol-sdk'

function makeEntry(kind: string, payload: unknown = {}): HistoryEntry {
  return {
    eventId: 'evt-1',
    kind,
    subject: 'identity:id-1:self:test',
    payload,
    timestamp: '2026-09-08T00:00:00Z',
  }
}

describe('summarizeActivityEntry', () => {
  it('summarizes identity.created', () => {
    expect(summarizeActivityEntry(makeEntry('identity.created'))).toBe(
      'You created your identity.',
    )
  })

  it('summarizes friend.requested/accepted/removed', () => {
    expect(summarizeActivityEntry(makeEntry('friend.requested'))).toBe(
      'You sent a friend request.',
    )
    expect(summarizeActivityEntry(makeEntry('friend.accepted'))).toBe(
      'You accepted a friend request.',
    )
    expect(summarizeActivityEntry(makeEntry('friend.removed'))).toBe('You removed a friend.')
  })

  it('includes the device label for identity.signing_key_added when present', () => {
    expect(
      summarizeActivityEntry(
        makeEntry('identity.signing_key_added', { device_label: 'Work laptop' }),
      ),
    ).toBe('New device added: Work laptop.')
  })

  it('falls back sensibly when identity.signing_key_added has no label', () => {
    expect(summarizeActivityEntry(makeEntry('identity.signing_key_added', {}))).toBe(
      'New device added.',
    )
  })

  it('summarizes identity.signing_key_revoked', () => {
    expect(summarizeActivityEntry(makeEntry('identity.signing_key_revoked'))).toBe(
      'Device access revoked.',
    )
  })

  it('summarizes guild events with the name/reason payload fields when present', () => {
    expect(summarizeActivityEntry(makeEntry('guild.created', { name: 'Celestial Forge' }))).toBe(
      'You created the guild Celestial Forge.',
    )
    expect(summarizeActivityEntry(makeEntry('guild.created', {}))).toBe('You created a guild.')
    expect(summarizeActivityEntry(makeEntry('guild.updated', { name: 'Celestial Forge' }))).toBe(
      "You updated Celestial Forge's settings.",
    )
    expect(summarizeActivityEntry(makeEntry('guild.role_defined', { name: 'Officer' }))).toBe(
      'You defined the role Officer.',
    )
    expect(summarizeActivityEntry(makeEntry('guild.role_deleted'))).toBe(
      'You deleted a guild role.',
    )
    expect(summarizeActivityEntry(makeEntry('guild.role_changed'))).toBe(
      "You changed a member's role.",
    )
    expect(summarizeActivityEntry(makeEntry('guild.owner_transferred'))).toBe(
      'You transferred guild ownership.',
    )
    expect(summarizeActivityEntry(makeEntry('guild.game_associated'))).toBe(
      'You associated an integrator with your guild.',
    )
    expect(summarizeActivityEntry(makeEntry('guild.member_added'))).toBe('You joined a guild.')
    expect(summarizeActivityEntry(makeEntry('guild.member_removed', { reason: 'left' }))).toBe(
      'You left a guild.',
    )
    expect(summarizeActivityEntry(makeEntry('guild.member_removed', { reason: 'removed' }))).toBe(
      'You removed a member from a guild.',
    )
    expect(summarizeActivityEntry(makeEntry('guild.favorite_games_updated'))).toBe(
      "You updated your guild's favorite integrators.",
    )
    expect(summarizeActivityEntry(makeEntry('guild.channel_created', { name: 'raids' }))).toBe(
      'You created the channel #raids.',
    )
    expect(summarizeActivityEntry(makeEntry('guild.channel_renamed', { name: 'raids' }))).toBe(
      'You renamed a channel to #raids.',
    )
    expect(summarizeActivityEntry(makeEntry('guild.channel_archived'))).toBe(
      'You archived a guild channel.',
    )
  })

  it('falls back to the raw kind for an unrecognized event, never throwing', () => {
    expect(summarizeActivityEntry(makeEntry('some.future.kind', { anything: 'here' }))).toBe(
      'some.future.kind',
    )
  })

  it('never throws on a malformed payload', () => {
    expect(() =>
      summarizeActivityEntry(makeEntry('identity.signing_key_added', 'not an object')),
    ).not.toThrow()
    expect(() => summarizeActivityEntry(makeEntry('identity.signing_key_added', null))).not.toThrow()
  })
})

describe('formatActivityTimestamp', () => {
  const now = new Date('2026-09-08T12:00:00Z')

  it('renders "just now" for anything under a minute old', () => {
    expect(formatActivityTimestamp('2026-09-08T11:59:45Z', now)).toBe('just now')
  })

  it('renders relative minutes for a recent timestamp', () => {
    expect(formatActivityTimestamp('2026-09-08T11:55:00Z', now)).toBe('5 minutes ago')
  })

  it('renders relative hours for an older-same-day timestamp', () => {
    expect(formatActivityTimestamp('2026-09-08T09:00:00Z', now)).toBe('3 hours ago')
  })

  it('renders relative days for a multi-day-old timestamp', () => {
    expect(formatActivityTimestamp('2026-09-06T12:00:00Z', now)).toBe('2 days ago')
  })
})
