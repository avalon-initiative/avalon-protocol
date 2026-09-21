import { beforeEach, describe, expect, it } from 'vitest'
import {
  countNewGuardianOf,
  isConversationUnread,
  loadConversationsLastSeen,
  loadGuardianOfSeen,
  markConversationSeen,
  markGuardianOfSeen,
} from './notifications'
import type { ConversationMessage } from '@avalon/sdk'

beforeEach(() => {
  localStorage.clear()
})

function makeMessage(overrides: Partial<ConversationMessage> = {}): ConversationMessage {
  return {
    id: 'm1',
    conversationId: 'c1',
    author: 'id-friend',
    body: 'hey',
    sentAt: '2026-01-02T00:00:00Z',
    ...overrides,
  }
}

describe('isConversationUnread', () => {
  it('is false when there is no last message at all', () => {
    expect(isConversationUnread('c1', null, 'id-self', {})).toBe(false)
  })

  it('is false when the caller sent the last message themselves', () => {
    expect(isConversationUnread('c1', makeMessage({ author: 'id-self' }), 'id-self', {})).toBe(false)
  })

  it('is true when never seen and the last message is from someone else', () => {
    expect(isConversationUnread('c1', makeMessage(), 'id-self', {})).toBe(true)
  })

  it('is false once seen at or after the last message', () => {
    const lastSeen = { c1: '2026-01-02T00:00:00Z' }
    expect(isConversationUnread('c1', makeMessage(), 'id-self', lastSeen)).toBe(false)
  })

  it('is true again once a newer message arrives after the last seen mark', () => {
    const lastSeen = { c1: '2026-01-01T00:00:00Z' }
    expect(isConversationUnread('c1', makeMessage({ sentAt: '2026-01-02T00:00:00Z' }), 'id-self', lastSeen)).toBe(
      true,
    )
  })
})

describe('markConversationSeen / loadConversationsLastSeen', () => {
  it('round-trips a seen mark through localStorage', () => {
    markConversationSeen('c1', '2026-01-02T00:00:00Z')
    expect(loadConversationsLastSeen()).toEqual({ c1: '2026-01-02T00:00:00Z' })
  })

  it('never moves a seen mark backwards', () => {
    markConversationSeen('c1', '2026-01-02T00:00:00Z')
    markConversationSeen('c1', '2026-01-01T00:00:00Z')
    expect(loadConversationsLastSeen().c1).toBe('2026-01-02T00:00:00Z')
  })
})

describe('countNewGuardianOf', () => {
  it('counts only ids not already marked seen', () => {
    expect(countNewGuardianOf(['a', 'b', 'c'], ['a'])).toBe(2)
  })

  it('is zero once everything currently listed has been seen', () => {
    expect(countNewGuardianOf(['a', 'b'], ['a', 'b'])).toBe(0)
  })
})

describe('markGuardianOfSeen / loadGuardianOfSeen', () => {
  it('round-trips seen ids through localStorage, deduplicated', () => {
    markGuardianOfSeen(['a', 'b'])
    markGuardianOfSeen(['b', 'c'])
    expect(new Set(loadGuardianOfSeen())).toEqual(new Set(['a', 'b', 'c']))
  })
})
