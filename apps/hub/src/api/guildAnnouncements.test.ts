import { beforeEach, describe, expect, it } from 'vitest'
import type { GuildAnnouncementAlert } from '@avalon/sdk'
import { countUnread, isUnread, loadLastSeen, markChannelSeen, previewBody } from './guildAnnouncements'

function alert(overrides: Partial<GuildAnnouncementAlert> = {}): GuildAnnouncementAlert {
  return {
    messageId: 'm-1',
    channelId: 'c-1',
    channelName: 'announcements',
    guildId: 'g-1',
    author: 'a-1',
    body: 'server maintenance tonight',
    sentAt: '2026-09-14T12:00:00Z',
    ...overrides,
  }
}

beforeEach(() => {
  localStorage.clear()
})

describe('isUnread / countUnread', () => {
  it('is unread when the channel has never been seen', () => {
    expect(isUnread(alert(), {})).toBe(true)
  })

  it('is unread when sent after the last-seen timestamp', () => {
    const lastSeen = { 'c-1': '2026-09-14T11:00:00Z' }
    expect(isUnread(alert({ sentAt: '2026-09-14T12:00:00Z' }), lastSeen)).toBe(true)
  })

  it('is read when sent at or before the last-seen timestamp', () => {
    const lastSeen = { 'c-1': '2026-09-14T12:00:00Z' }
    expect(isUnread(alert({ sentAt: '2026-09-14T12:00:00Z' }), lastSeen)).toBe(false)
    expect(isUnread(alert({ sentAt: '2026-09-14T11:00:00Z' }), lastSeen)).toBe(false)
  })

  it('counts only the unread alerts across multiple channels', () => {
    const alerts = [
      alert({ channelId: 'c-1', sentAt: '2026-09-14T12:00:00Z' }),
      alert({ channelId: 'c-2', sentAt: '2026-09-14T12:00:00Z' }),
      alert({ channelId: 'c-1', messageId: 'm-2', sentAt: '2026-09-13T00:00:00Z' }),
    ]
    const lastSeen = { 'c-1': '2026-09-14T00:00:00Z' }
    // c-1's newer post is unread, its older post is read, c-2 has never
    // been seen at all — 2 unread total.
    expect(countUnread(alerts, lastSeen)).toBe(2)
  })
})

describe('markChannelSeen / loadLastSeen', () => {
  it('persists a channel as seen and is readable back', () => {
    markChannelSeen('c-1', '2026-09-14T12:00:00Z')
    expect(loadLastSeen()).toEqual({ 'c-1': '2026-09-14T12:00:00Z' })
  })

  it('never moves a channel backwards to an earlier seen time', () => {
    markChannelSeen('c-1', '2026-09-14T12:00:00Z')
    markChannelSeen('c-1', '2026-09-14T10:00:00Z')
    expect(loadLastSeen()['c-1']).toBe('2026-09-14T12:00:00Z')
  })

  it('advances when marked seen at a later time', () => {
    markChannelSeen('c-1', '2026-09-14T10:00:00Z')
    markChannelSeen('c-1', '2026-09-14T12:00:00Z')
    expect(loadLastSeen()['c-1']).toBe('2026-09-14T12:00:00Z')
  })

  it('returns an empty object when nothing has ever been stored', () => {
    expect(loadLastSeen()).toEqual({})
  })
})

describe('previewBody', () => {
  it('leaves a short body untouched', () => {
    expect(previewBody('short message')).toBe('short message')
  })

  it('truncates a long body with an ellipsis', () => {
    const long = 'x'.repeat(120)
    const preview = previewBody(long)
    expect(preview.length).toBe(80)
    expect(preview.endsWith('…')).toBe(true)
  })

  it('trims surrounding whitespace before measuring length', () => {
    expect(previewBody('  hi  ')).toBe('hi')
  })
})
