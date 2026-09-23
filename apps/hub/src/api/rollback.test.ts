// The wrappers delegate to AccountSession unchanged; the helpers are pure.
import { describe, expect, it, vi } from 'vitest'
import type { AccountSession } from '@avalon-initiative/protocol-sdk'
import {
  errorCode,
  reverseRollbackEvent,
  rollbackCandidates,
  rollbackErrorMessage,
  toRfc3339Utc,
} from './rollback'

function fakeSession() {
  return {
    rollbackCandidates: vi.fn().mockResolvedValue({ candidates: [] }),
    reverseRollbackEvent: vi.fn().mockResolvedValue('rev-1'),
  } as unknown as AccountSession & {
    rollbackCandidates: ReturnType<typeof vi.fn>
    reverseRollbackEvent: ReturnType<typeof vi.fn>
  }
}

describe('rollback wrappers', () => {
  it('forwards since to rollbackCandidates', async () => {
    const s = fakeSession()
    await rollbackCandidates(s, '2026-01-01T00:00:00.000Z')
    expect(s.rollbackCandidates).toHaveBeenCalledWith('2026-01-01T00:00:00.000Z')
  })

  it('forwards event id and the same since to reverseRollbackEvent', async () => {
    const s = fakeSession()
    await expect(reverseRollbackEvent(s, 'e1', '2026-01-01T00:00:00.000Z')).resolves.toBe('rev-1')
    expect(s.reverseRollbackEvent).toHaveBeenCalledWith('e1', '2026-01-01T00:00:00.000Z')
  })
})

describe('toRfc3339Utc', () => {
  it('returns a UTC ISO string for a local value', () => {
    expect(toRfc3339Utc('2026-01-01T10:30')).toBe(new Date('2026-01-01T10:30').toISOString())
  })
  it('returns null for empty or invalid input', () => {
    expect(toRfc3339Utc('')).toBeNull()
    expect(toRfc3339Utc('nope')).toBeNull()
  })
})

describe('rollbackErrorMessage', () => {
  it.each([
    'ROLLBACK_NO_COMPLETED_RECOVERY',
    'INVALID_ROLLBACK_WINDOW',
    'ROLLBACK_EVENT_NOT_ELIGIBLE',
    'ROLLBACK_NOT_REVERSIBLE',
    'ROLLBACK_ALREADY_REVERSED',
  ])('has a specific message for %s', (code) => {
    expect(rollbackErrorMessage(code)).not.toBe(rollbackErrorMessage('SOMETHING_ELSE'))
  })
  it('falls back to a generic message', () => {
    expect(rollbackErrorMessage(undefined)).toBe(rollbackErrorMessage('UNKNOWN'))
  })
})

describe('errorCode', () => {
  it('reads code off an Error', () => {
    expect(errorCode(Object.assign(new Error('x'), { code: 'C' }))).toBe('C')
    expect(errorCode('str')).toBeUndefined()
  })
})
