import { describe, expect, it } from 'vitest'
import { MESSAGE_BODY_MAX_CHARS, toOldestFirst, validateComposerBody } from './guildChat'
import type { MessageResponse } from '@avalon/api-client'

describe('validateComposerBody', () => {
  it('rejects an empty body', () => {
    expect(validateComposerBody('').valid).toBe(false)
  })

  it('rejects a whitespace-only body', () => {
    expect(validateComposerBody('   ').valid).toBe(false)
  })

  it('accepts a normal body', () => {
    const result = validateComposerBody('hello guild')
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
    const messages: MessageResponse[] = [
      { id: '3', channel_id: 'c', author: 'a', body: 'third', sent_at: 't3' },
      { id: '2', channel_id: 'c', author: 'a', body: 'second', sent_at: 't2' },
      { id: '1', channel_id: 'c', author: 'a', body: 'first', sent_at: 't1' },
    ]
    expect(toOldestFirst(messages).map((m) => m.id)).toEqual(['1', '2', '3'])
  })

  it('does not mutate the input array', () => {
    const messages: MessageResponse[] = [
      { id: '2', channel_id: 'c', author: 'a', body: 'second', sent_at: 't2' },
      { id: '1', channel_id: 'c', author: 'a', body: 'first', sent_at: 't1' },
    ]
    toOldestFirst(messages)
    expect(messages.map((m) => m.id)).toEqual(['2', '1'])
  })
})
