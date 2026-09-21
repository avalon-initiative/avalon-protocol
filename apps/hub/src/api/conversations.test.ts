import { describe, expect, it } from 'vitest'
import {
  MESSAGE_BODY_MAX_CHARS,
  otherParticipants,
  toOldestFirst,
  validateComposerBody,
} from './conversations'
import type { ConversationMessage, Conversation } from '@avalon/sdk'

describe('validateComposerBody', () => {
  it('rejects an empty body', () => {
    expect(validateComposerBody('').valid).toBe(false)
  })

  it('rejects a whitespace-only body', () => {
    expect(validateComposerBody('   ').valid).toBe(false)
  })

  it('accepts a normal body', () => {
    const result = validateComposerBody('hello there')
    expect(result.valid).toBe(true)
    expect(result.charCount).toBe(11)
    expect(result.remaining).toBe(MESSAGE_BODY_MAX_CHARS - 11)
  })

  it('accepts a body exactly at the cap', () => {
    expect(validateComposerBody('a'.repeat(MESSAGE_BODY_MAX_CHARS)).valid).toBe(true)
  })

  it('rejects a body one character over the cap', () => {
    expect(validateComposerBody('a'.repeat(MESSAGE_BODY_MAX_CHARS + 1)).valid).toBe(false)
  })
})

describe('toOldestFirst', () => {
  it('reverses a newest-first page into oldest-first order', () => {
    const messages: ConversationMessage[] = [
      { id: '3', conversationId: 'c', author: 'a', body: 'third', sentAt: 't3' },
      { id: '2', conversationId: 'c', author: 'a', body: 'second', sentAt: 't2' },
      { id: '1', conversationId: 'c', author: 'a', body: 'first', sentAt: 't1' },
    ]
    expect(toOldestFirst(messages).map((m) => m.id)).toEqual(['1', '2', '3'])
  })

  it('does not mutate the input array', () => {
    const messages: ConversationMessage[] = [
      { id: '2', conversationId: 'c', author: 'a', body: 'second', sentAt: 't2' },
      { id: '1', conversationId: 'c', author: 'a', body: 'first', sentAt: 't1' },
    ]
    toOldestFirst(messages)
    expect(messages.map((m) => m.id)).toEqual(['2', '1'])
  })
})

describe('otherParticipants', () => {
  it('excludes the caller from a 1:1 conversation', () => {
    const conversation: Conversation = { id: 'c1', participants: ['self', 'friend'] }
    expect(otherParticipants(conversation, 'self')).toEqual(['friend'])
  })

  it('returns every other participant in a group conversation', () => {
    const conversation: Conversation = {
      id: 'c1',
      participants: ['self', 'a', 'b'],
    }
    expect(otherParticipants(conversation, 'self')).toEqual(['a', 'b'])
  })
})
