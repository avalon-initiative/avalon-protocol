import { describe, expect, it } from 'vitest'
import { MESSAGE_BODY_MAX_CHARS, toOldestFirst, validateComposerBody } from './guildChat'
import type { GuildMessage } from '@avalon-initiative/protocol-sdk'

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
    const messages: GuildMessage[] = [
      { id: '3', channelId: 'c', author: 'a', body: 'third', sentAt: 't3' },
      { id: '2', channelId: 'c', author: 'a', body: 'second', sentAt: 't2' },
      { id: '1', channelId: 'c', author: 'a', body: 'first', sentAt: 't1' },
    ]
    expect(toOldestFirst(messages).map((m) => m.id)).toEqual(['1', '2', '3'])
  })

  it('does not mutate the input array', () => {
    const messages: GuildMessage[] = [
      { id: '2', channelId: 'c', author: 'a', body: 'second', sentAt: 't2' },
      { id: '1', channelId: 'c', author: 'a', body: 'first', sentAt: 't1' },
    ]
    toOldestFirst(messages)
    expect(messages.map((m) => m.id)).toEqual(['2', '1'])
  })
})
